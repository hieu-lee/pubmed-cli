use crate::args::{Args, global_opt, opt_arity};
use crate::cmd;
use crate::enrich;
use crate::eutils;
use crate::fields;
use crate::filter::Filters;
use crate::http::Ctx;
use crate::model::*;
use crate::render;
use anyhow::{Result, bail};
use futures::future::{LocalBoxFuture, join_all};
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Tok {
    W(String),
    Pipe,
    Semi,
}

pub type Dispatch = for<'a> fn(&'a Ctx, &'a str, &'a Args, Option<Set>) -> LocalBoxFuture<'a, Result<Set>>;
pub type Finish = for<'a> fn(&'a Ctx, Set, &'a Args) -> LocalBoxFuture<'a, Result<String>>;

#[derive(Clone, Copy)]
pub struct Engine {
    pub dispatch: Dispatch,
    pub finish: Finish,
    pub known: fn(&str) -> bool,
}

fn prod_dispatch<'a>(ctx: &'a Ctx, name: &'a str, a: &'a Args, input: Option<Set>) -> LocalBoxFuture<'a, Result<Set>> {
    Box::pin(cmd::dispatch(ctx, name, a, input))
}

fn prod_finish<'a>(ctx: &'a Ctx, set: Set, a: &'a Args) -> LocalBoxFuture<'a, Result<String>> {
    Box::pin(finish(ctx, set, a))
}

const PROD: Engine = Engine { dispatch: prod_dispatch, finish: prod_finish, known: cmd::is_command };

static FAILED: AtomicBool = AtomicBool::new(false);

pub fn mark_failed() {
    FAILED.store(true, Ordering::Relaxed);
}

pub fn failed() -> bool {
    FAILED.load(Ordering::Relaxed)
}

pub fn err_line(e: &anyhow::Error) -> String {
    let m = crate::http::redact(&format!("{e:#}"));
    format!("error: {}", m.split_whitespace().collect::<Vec<_>>().join(" "))
}

pub fn clip(s: &str, n: usize) -> String {
    match s.char_indices().nth(n) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s.to_string(),
    }
}

fn flush(out: &mut Vec<Tok>, cur: &mut String, have: &mut bool, lit: &mut bool) {
    let literal = std::mem::take(lit);
    if std::mem::take(have) {
        let w = std::mem::take(cur);
        out.push(match w.as_str() {
            "::" if !literal => Tok::Pipe,
            "++" if !literal => Tok::Semi,
            _ => Tok::W(w),
        });
    }
}

pub fn lex_str(line: &str) -> Result<Vec<Tok>> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut have = false;
    let mut lit = false;
    let mut quote: Option<char> = None;
    let mut it = line.chars().peekable();
    while let Some(c) = it.next() {
        if let Some(q) = quote {
            if c == q {
                quote = None;
            } else if c == '\\' && q == '"' && matches!(it.peek(), Some('"' | '\\')) {
                cur.push(it.next().unwrap());
            } else {
                cur.push(c);
            }
            continue;
        }
        match c {
            '"' | '\'' => {
                quote = Some(c);
                have = true;
                lit = true;
            }
            '\\' => {
                if let Some(n) = it.next() {
                    cur.push(n);
                    have = true;
                    lit = true;
                }
            }
            '|' | ';' => {
                flush(&mut out, &mut cur, &mut have, &mut lit);
                out.push(if c == '|' { Tok::Pipe } else { Tok::Semi });
            }
            c if c.is_whitespace() => flush(&mut out, &mut cur, &mut have, &mut lit),
            c => {
                cur.push(c);
                have = true;
            }
        }
    }
    if quote.is_some() {
        bail!("unterminated quote");
    }
    flush(&mut out, &mut cur, &mut have, &mut lit);
    Ok(out)
}

fn is_pipeline_arg(s: &str) -> bool {
    s.contains(char::is_whitespace)
        && (s.split_whitespace().next().is_some_and(cmd::is_command) || (s.starts_with('-') && s.split_whitespace().any(cmd::is_command)))
}

fn skip_globals(toks: &[String]) -> Option<usize> {
    let mut i = 0;
    while i < toks.len() && toks[i].starts_with('-') {
        let n = global_opt(&toks[i])?;
        if n == 1 && toks.get(i + 1).is_none_or(|w| w.starts_with('-')) {
            return None;
        }
        i += 1 + n;
    }
    Some(i)
}

fn only_options(toks: &[String]) -> bool {
    skip_globals(toks) == Some(toks.len())
}

fn overlay(toks: Vec<Tok>, front: &[String], back: &[String]) -> Vec<Tok> {
    let mut tasks = split_on(toks, &Tok::Semi);
    tasks.retain(|t| !t.is_empty());
    for task in tasks.iter_mut() {
        let mut stages = split_on(std::mem::take(task), &Tok::Pipe);
        if let Some(last) = stages.last_mut() {
            let at = match last.iter().position(|t| matches!(t, Tok::W(_))) {
                Some(i) if matches!(&last[i], Tok::W(w) if cmd::is_command(w)) => i + 1,
                _ => 0,
            };
            last.splice(at..at, front.iter().cloned().map(Tok::W));
            last.extend(back.iter().cloned().map(Tok::W));
        }
        let mut joined = Vec::new();
        for (i, s) in stages.into_iter().enumerate() {
            if i > 0 {
                joined.push(Tok::Pipe);
            }
            joined.extend(s);
        }
        *task = joined;
    }
    let mut out = Vec::new();
    for (i, t) in tasks.into_iter().enumerate() {
        if i > 0 {
            out.push(Tok::Semi);
        }
        out.extend(t);
    }
    out
}

pub fn lex(argv: Vec<String>) -> Result<Vec<Tok>> {
    if let [one] = argv.as_slice()
        && is_pipeline_arg(one)
    {
        return lex_str(one);
    }
    if let Some(p) = (0..argv.len()).find(|&i| {
        is_pipeline_arg(&argv[i]) && argv.len() > 1 && only_options(&argv[..i]) && only_options(&argv[i + 1..])
    }) {
        return Ok(overlay(lex_str(&argv[p])?, &argv[..p], &argv[p + 1..]));
    }
    let mut value_next = false;
    Ok(argv
        .into_iter()
        .map(|t| {
            let tok = match t.as_str() {
                "|" | "::" if !value_next => Tok::Pipe,
                ";" | "++" if !value_next => Tok::Semi,
                _ => Tok::W(t),
            };
            value_next = matches!(&tok, Tok::W(w) if opt_arity(w) == 1);
            tok
        })
        .collect())
}

pub fn words(toks: &[Tok]) -> Vec<String> {
    toks.iter()
        .filter_map(|t| match t {
            Tok::W(w) => Some(w.clone()),
            _ => None,
        })
        .collect()
}

pub fn split_on(toks: Vec<Tok>, sep: &Tok) -> Vec<Vec<Tok>> {
    let mut out = vec![Vec::new()];
    for t in toks {
        if t == *sep {
            out.push(Vec::new());
        } else {
            out.last_mut().unwrap().push(t);
        }
    }
    out
}

const SECRET_OPTS: &[&str] = &["key", "email"];

pub(crate) fn secret_flag(w: &str) -> Option<(&str, Option<&str>)> {
    let body = w.strip_prefix('-')?.trim_start_matches('-');
    let (name, val) = match body.split_once('=') {
        Some((n, v)) => (n, Some(v)),
        None => (body, None),
    };
    SECRET_OPTS.contains(&name.to_ascii_lowercase().as_str()).then_some((name, val))
}

pub fn redact(line: &str) -> String {
    match lex_str(line) {
        Ok(t) => crate::http::redact(&task_text(&t)),
        Err(_) => "<unparsable>".into(),
    }
}

fn task_text(toks: &[Tok]) -> String {
    let mut s = String::new();
    let mut hide = false;
    for t in toks {
        if !s.is_empty() {
            s.push(' ');
        }
        match t {
            Tok::W(_) if hide => {
                s.push_str("***");
                hide = false;
            }
            Tok::W(w) => match secret_flag(w) {
                Some((_, Some(_))) => {
                    s.push_str(w.split_once('=').map_or(w.as_str(), |x| x.0));
                    s.push_str("=***");
                }
                Some((_, None)) => {
                    hide = true;
                    s.push_str(w);
                }
                None if w.is_empty() || w.contains(char::is_whitespace) => {
                    s.push('"');
                    match lex_str(w) {
                        Ok(inner) if w.contains(char::is_whitespace) => s.push_str(&task_text(&inner)),
                        Err(_) => {
                            if let Some(first) = w.split_whitespace().next().filter(|x| cmd::is_command(x)) {
                                s.push_str(first);
                                s.push(' ');
                            }
                            s.push_str("<unparsable>");
                        }
                        Ok(_) => s.push_str(w),
                    }
                    s.push('"');
                }
                None => s.push_str(w),
            },
            Tok::Pipe => s.push('|'),
            Tok::Semi => s.push(';'),
        }
    }
    s
}

#[derive(Debug)]
struct Stage {
    name: String,
    args: Args,
}

fn id_like(t: &str) -> bool {
    let l = t.to_ascii_lowercase();
    t.bytes().all(|b| b.is_ascii_digit())
        || l.starts_with("doi:")
        || l.starts_with("pmc:")
        || l.strip_prefix("pmid:").is_some_and(|r| !r.is_empty() && r.bytes().all(|b| b.is_ascii_digit()))
        || l.strip_prefix("pmc").is_some_and(|r| !r.is_empty() && r.bytes().all(|b| b.is_ascii_digit()))
        || (l.starts_with("10.") && l.contains('/'))
}

fn hoist_command(e: &Engine, words: &mut [String]) {
    let Some(i) = skip_globals(words) else { return };
    if i > 0 && i < words.len() && (e.known)(&words[i]) {
        words[..=i].rotate_right(1);
    }
}

fn parse_stage(e: &Engine, first: bool, mut words: Vec<String>) -> Result<Stage> {
    if words.is_empty() {
        bail!("empty pipeline stage");
    }
    if first {
        hoist_command(e, &mut words);
    }
    if (e.known)(&words[0]) {
        let name = words.remove(0);
        return Ok(Stage { name, args: Args::parse(&words)? });
    }
    if !first {
        return Err(cmd::unknown(&words[0]));
    }
    if words.len() == 1 && !words[0].starts_with('-') && cmd::suggest(&words[0]).is_some() {
        return Err(cmd::unknown(&words[0]));
    }
    let args = Args::parse(&words)?;
    let name = if !args.pos.is_empty() && args.pos.iter().all(|p| id_like(p)) { "show" } else { "search" };
    Ok(Stage { name: name.into(), args })
}

fn parse_task(e: &Engine, toks: Vec<Tok>) -> Result<Vec<Stage>> {
    split_on(toks, &Tok::Pipe)
        .into_iter()
        .enumerate()
        .map(|(i, seg)| parse_stage(e, i == 0, words(&seg)))
        .collect()
}

async fn run_stages(e: Engine, ctx: &Ctx, mut stages: Vec<Stage>) -> Result<String> {
    let multi = stages.len() > 1;
    let last = stages.pop().expect("parse_task yields at least one stage");
    let tag = |name: &str, err: anyhow::Error| if multi { err.context(name.to_string()) } else { err };
    let mut set: Option<Set> = None;
    for s in &stages {
        set = Some((e.dispatch)(ctx, &s.name, &s.args, set.take()).await.map_err(|er| tag(&s.name, er))?);
    }
    let out = (e.dispatch)(ctx, &last.name, &last.args, set.take()).await.map_err(|er| tag(&last.name, er))?;
    let mut fin = last.args.clone();
    fin.flags.push(format!("cmd:{}", canonical(&last.name)));
    for s in &stages {
        carry_query_terms(&mut fin, s);
    }
    for s in stages.iter().rev() {
        carry_render_opts(&mut fin, s);
    }
    (e.finish)(ctx, out, &fin).await
}

const RENDER_OPTS: &[&str] = &["abs", "authors", "budget", "fields", "snip", "snips"];
const RENDER_FLAGS: &[&str] = &["quiet", "no-header"];

fn effective_fmt(a: &Args) -> Option<String> {
    match a.get("fmt") {
        None | Some("auto") if a.flag("json") => Some("json".to_string()),
        Some(f) => Some(f.to_string()),
        None if a.explicit("json") => Some("auto".to_string()),
        None => None,
    }
}

fn carry_render_opts(fin: &mut Args, s: &Stage) {
    if effective_fmt(fin).is_none()
        && let Some(f) = effective_fmt(&s.args)
    {
        fin.opts.push(("fmt".to_string(), f));
    }
    for k in RENDER_OPTS {
        if fin.opts.iter().any(|(fk, _)| fk == k) {
            continue;
        }
        let mut vals = s.args.opts.iter().filter(|(sk, _)| sk == k).map(|(_, v)| v);
        if *k == "fields" {
            fin.opts.extend(vals.map(|v| (k.to_string(), v.clone())));
        } else if let Some(v) = vals.next_back() {
            fin.opts.push((k.to_string(), v.clone()));
        }
    }
    for k in RENDER_FLAGS {
        if fin.explicit(k) {
            continue;
        }
        if s.args.flag(k) {
            fin.flags.push(k.to_string());
        } else if s.args.explicit(k) {
            fin.opts.push((k.to_string(), "false".into()));
        }
    }
}

fn canonical(name: &str) -> &str {
    match name {
        "s" => "search",
        "c" => "count",
        "get" => "show",
        "rel" => "related",
        "cb" => "citedby",
        "ft" => "fulltext",
        "j" => "journal",
        "head" => "top",
        "run" => "batch",
        n => n,
    }
}

const TERM_KEYS: &[&str] = &["grep", "grep-ti", "grep-ab", "phrase", "fz", "all", "any", "ti", "tiab", "ab", "near"];

fn carry_query_terms(fin: &mut Args, s: &Stage) {
    for (k, v) in &s.args.opts {
        if TERM_KEYS.contains(&k.as_str()) && !fin.opts.iter().any(|(fk, fv)| fk == k && fv == v) {
            fin.opts.push((k.clone(), v.clone()));
        }
    }
    if matches!(s.name.as_str(), "search" | "s") {
        for f in ["ti", "ab", "tiab"] {
            if s.args.flags.iter().any(|x| x == f) && !fin.flags.iter().any(|x| x == f) {
                fin.flags.push(f.to_string());
            }
        }
        for w in &s.args.pos {
            if !fin.opts.iter().any(|(fk, fv)| fk == "query" && fv == w) {
                fin.opts.push(("query".into(), w.clone()));
            }
        }
    }
}

async fn run_task(e: Engine, ctx: &Ctx, toks: Vec<Tok>) -> Result<String> {
    run_stages(e, ctx, parse_task(&e, toks)?).await
}

async fn exec(e: Engine, ctx: &Ctx, toks: Vec<Tok>) -> Result<String> {
    let mut tasks = split_on(toks, &Tok::Semi);
    tasks.retain(|t| !t.is_empty());
    if tasks.len() <= 1 {
        return match tasks.pop() {
            Some(t) => run_task(e, ctx, t).await,
            None => Ok(String::new()),
        };
    }
    let done = join_all(tasks.into_iter().map(|t| async move {
        let head = clip(&crate::http::redact(&task_text(&t)), 60);
        (head, run_task(e, ctx, t).await)
    }))
    .await;
    let mut out = String::new();
    for (i, (head, r)) in done.into_iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        out.push_str("## ");
        out.push_str(&head);
        out.push('\n');
        match r {
            Ok(s) => out.push_str(s.trim_end_matches('\n')),
            Err(er) => {
                mark_failed();
                out.push_str(&err_line(&er));
            }
        }
    }
    Ok(out)
}

pub async fn run_toks(ctx: &Ctx, toks: Vec<Tok>) -> Result<String> {
    exec(PROD, ctx, toks).await
}

pub async fn run_str(ctx: &Ctx, line: &str) -> Result<String> {
    run_toks(ctx, lex_str(line)?).await
}

fn wants_arts(fields: &[String], fmt: Option<&str>) -> bool {
    fmt != Some("ids") && !fields::is_pmid_only(fields)
}

async fn finish(ctx: &Ctx, mut set: Set, a: &Args) -> Result<String> {
    if set.table.is_none() && set.text.is_none() && !set.ids.is_empty() {
        let mut fields = a.csv("fields");
        if fields.is_empty() {
            fields = set.fields.clone().unwrap_or_default();
        }
        if set.arts.is_empty() && wants_arts(&fields, a.get("fmt")) {
            eutils::load(ctx, &mut set).await?;
        }
        if !set.arts.is_empty() {
            let mut need = fields::need_for(&set, a);
            if let Ok(f) = Filters::from_args(a) {
                need = need.or(f.need());
            }
            if need != Need::default()
                && let Err(e) = enrich::apply(ctx, &mut set.arts, need).await
            {
                let m = crate::http::redact(&format!("{e:#}"));
                eprintln!("warning: enrich failed: {m}");
                set.notes.push(format!("enrich failed: {m}"));
            }
        }
    }
    for n in &mut set.notes {
        *n = crate::http::redact(n);
    }
    render::render(&set, a)
}

#[cfg(test)]
mod tests {
    #[test]
    fn pmid_prefix_is_id_like() {
        for t in ["pmid:5410310", "PMID:5410310", "pmc:PMC1", "doi:10.1/x", "5410310"] {
            assert!(id_like(t), "{t}");
        }
        assert!(!id_like("pmid:"));
        assert!(!id_like("pmid:abc"));
    }

    use super::*;
    use crate::http::Config;
    use anyhow::anyhow;
    use std::time::{Duration, Instant};

    fn parses_line(line: &str) -> Result<()> {
        for task in split_on(lex_str(line)?, &Tok::Semi) {
            if !task.is_empty() {
                parse_task(&PROD, task)?;
            }
        }
        Ok(())
    }

    #[test]
    fn global_options_before_a_quoted_pipeline_apply_to_every_task() {
        let argv: Vec<String> = ["--no-cache", "--fmt", "tsv", "journal nature ++ search x :: top 2"].iter().map(|s| s.to_string()).collect();
        let toks = lex(argv).unwrap();
        let tasks = split_on(toks, &Tok::Semi);
        assert_eq!(tasks.len(), 2);
        let w = |t: &[Tok]| words(t);
        assert_eq!(w(&tasks[0]), ["journal", "--no-cache", "--fmt", "tsv", "nature"]);
        let stages = split_on(tasks[1].clone(), &Tok::Pipe);
        assert_eq!(w(&stages[0]), ["search", "x"]);
        assert_eq!(w(&stages[1]), ["top", "--no-cache", "--fmt", "tsv", "2"]);
        let one: Vec<String> = ["--key", "K", "show 1 ++ show 2"].iter().map(|s| s.to_string()).collect();
        assert_eq!(split_on(lex(one).unwrap(), &Tok::Semi).len(), 2);
        let not_pipeline: Vec<String> = ["--grep", "show me"].iter().map(|s| s.to_string()).collect();
        assert_eq!(words(&lex(not_pipeline).unwrap()), ["--grep", "show me"]);
    }

    #[test]
    fn trailing_options_after_a_quoted_pipeline_apply_to_the_final_stage() {
        let argv = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let toks = lex(argv(&["top 2 11 12 13 -f pmid | top 1", "--fmt", "ids"])).unwrap();
        let stages = split_on(toks, &Tok::Pipe);
        assert_eq!(stages.len(), 2);
        assert_eq!(words(&stages[0]), ["top", "2", "11", "12", "13", "-f", "pmid"]);
        assert_eq!(words(&stages[1]), ["top", "1", "--fmt", "ids"]);
        let both = lex(argv(&["--quiet", "journal nature ++ search x :: top 2", "--fmt", "tsv"])).unwrap();
        let tasks = split_on(both, &Tok::Semi);
        assert_eq!(words(&tasks[0]), ["journal", "--quiet", "nature", "--fmt", "tsv"]);
        let stages = split_on(tasks[1].clone(), &Tok::Pipe);
        assert_eq!(words(&stages[1]), ["top", "--quiet", "2", "--fmt", "tsv"]);
        let not_global = lex(argv(&["show me", "--grep", "x"])).unwrap();
        assert_eq!(words(&not_global), ["show me", "--grep", "x"]);
        let search = lex(argv(&["search", "show me", "-n", "3"])).unwrap();
        assert_eq!(words(&search), ["search", "show me", "-n", "3"]);
    }

    #[test]
    fn explicit_json_false_overrides_inherited_json() {
        let parse = |s: &str| Args::parse(&s.split(' ').map(String::from).collect::<Vec<_>>()).unwrap();
        assert_eq!(effective_fmt(&parse("--json")), Some("json".into()));
        assert_eq!(effective_fmt(&parse("--json=false")), Some("auto".into()));
        assert_eq!(effective_fmt(&parse("--fmt ids")), Some("ids".into()));
        assert_eq!(effective_fmt(&parse("-n 1")), None);
        let mut fin = parse("--json=false");
        let stage = Stage { name: "top".into(), args: parse("--json -f pmid") };
        carry_render_opts(&mut fin, &stage);
        assert_eq!(effective_fmt(&fin), Some("auto".into()));
        assert!(fin.get("fmt").is_none());
        let mut plain = parse("-n 1");
        carry_render_opts(&mut plain, &stage);
        assert_eq!(effective_fmt(&plain), Some("json".into()));
    }

    #[test]
    fn help_examples_parse() {
        let mut topics: Vec<Option<&str>> = vec![None];
        for t in ["cmds", "query", "filters", "fields", "regex", "recency", "pipe", "examples"] {
            topics.push(Some(t));
        }
        topics.extend(cmd::NAMES.iter().map(|n| Some(*n)));
        let mut checked = 0;
        for topic in topics {
            for line in crate::help::text(topic).lines() {
                let mut rest = line;
                while let Some(at) = rest.find("pm ") {
                    let (before, after) = rest.split_at(at);
                    let boundary = before.chars().last().is_none_or(|c| c.is_whitespace() || c == ':');
                    let after = &after[3..];
                    let end = after.find(" pm ").unwrap_or(after.len());
                    let seg = after[..end].trim();
                    rest = &after[end..];
                    let first = seg.split_whitespace().next().unwrap_or("");
                    if !boundary || matches!(first, "help" | "batch") || seg.contains("|<") {
                        continue;
                    }
                    let seg = seg.split(" ; then").next().unwrap_or(seg);
                    if let Err(e) = parses_line(seg) {
                        panic!("help example does not parse: pm {seg}: {e}");
                    }
                    checked += 1;
                }
            }
        }
        assert!(checked >= 30, "only {checked} examples checked");
        for l in ["search a -n 3", "count b"] {
            parses_line(l).unwrap();
        }
    }

    #[test]
    fn quoted_separators_stay_literal() {
        let t = lex_str("top 1 39814420 | filter --grep \"::\" --fmt ids ; x \\++ y").unwrap();
        let w: Vec<String> = t.iter().map(|t| match t { Tok::W(w) => w.clone(), Tok::Pipe => "<P>".into(), Tok::Semi => "<S>".into() }).collect();
        assert_eq!(w, ["top", "1", "39814420", "<P>", "filter", "--grep", "::", "--fmt", "ids", "<S>", "x", "++", "y"]);
        let t = lex_str("a :: b ++ c").unwrap();
        assert!(matches!(t[1], Tok::Pipe) && matches!(t[3], Tok::Semi));
    }

    #[test]
    fn separator_after_value_option_is_literal() {
        let argv = |s: &str| s.split(' ').map(String::from).collect::<Vec<_>>();
        let w = |t: Vec<Tok>| t.iter().map(|t| match t { Tok::W(w) => w.clone(), Tok::Pipe => "<P>".into(), Tok::Semi => "<S>".into() }).collect::<Vec<_>>();
        assert_eq!(w(lex(argv("show 1 :: filter --grep :: --fmt ids")).unwrap()), ["show", "1", "<P>", "filter", "--grep", "::", "--fmt", "ids"]);
        assert_eq!(w(lex(argv("search x -n ++ search y")).unwrap()), ["search", "x", "-n", "++", "search", "y"]);
        assert_eq!(w(lex(argv("search x --quiet :: top 1")).unwrap()), ["search", "x", "--quiet", "<P>", "top", "1"]);
        assert_eq!(w(lex(argv("search --ti ++ search y")).unwrap()), ["search", "--ti", "<S>", "search", "y"]);
    }

    #[test]
    fn canonical_names() {
        assert_eq!(canonical("s"), "search");
        assert_eq!(canonical("head"), "top");
        assert_eq!(canonical("sort"), "sort");
    }

    #[test]
    fn id_alias_counts_as_pmid_only() {
        let f = |s: &str| s.split(',').map(String::from).collect::<Vec<_>>();
        assert!(!wants_arts(&f("id"), None));
        assert!(!wants_arts(&f("pmid,id"), None));
        assert!(wants_arts(&f("id,title"), None));
        assert!(wants_arts(&f("+id"), None));
        assert!(wants_arts(&[], None));
    }

    #[test]
    fn nested_pipeline_strings_are_redacted_in_task_text() {
        let toks = lex(vec!["batch".into(), "journal nature --key SECRET".into(), "++".into(), "journal".into(), "nature".into()]).unwrap();
        let tasks = split_on(toks, &Tok::Semi);
        let head = task_text(&tasks[0]);
        assert_eq!(head, r#"batch "journal nature --key ***""#);
        assert!(!head.contains("SECRET"));
        let eq = lex(vec!["batch".into(), "journal nature --key=SECRET -n 2".into()]).unwrap();
        assert!(!task_text(&eq).contains("SECRET"));
    }

    #[test]
    fn unparsable_nested_args_are_never_echoed() {
        let toks = lex(vec!["batch".into(), "search cancer --key DEMO_SECRET 'oops".into(), "++".into(), "help".into(), "cache".into()]).unwrap();
        let tasks = split_on(toks, &Tok::Semi);
        let head = task_text(&tasks[0]);
        assert_eq!(head, r#"batch "search <unparsable>""#);
        assert!(!head.contains("DEMO_SECRET"));
        let junk = lex(vec!["batch".into(), "zzz --key=S3 \"x".into()]).unwrap();
        assert_eq!(task_text(&junk), r#"batch "<unparsable>""#);
    }

    #[test]
    fn query_terms_forward_all_values_and_short_search_name() {
        let st = |name: &str, line: &str| Stage { name: name.into(), args: Args::parse(&line.split_whitespace().map(String::from).collect::<Vec<_>>()).unwrap() };
        let mut fin = Args::parse(&["-f".to_string(), "snip".to_string()]).unwrap();
        carry_query_terms(&mut fin, &st("s", "statin --grep alpha --grep beta --grep alpha"));
        carry_query_terms(&mut fin, &st("search", "statin --phrase x"));
        assert_eq!(fin.all("grep"), ["alpha", "beta"]);
        assert_eq!(fin.all("phrase"), ["x"]);
        assert_eq!(fin.all("query"), ["statin"]);
    }

    fn w(s: &str) -> Tok {
        Tok::W(s.into())
    }

    fn fake<'a>(_: &'a Ctx, name: &'a str, a: &'a Args, input: Option<Set>) -> LocalBoxFuture<'a, Result<Set>> {
        Box::pin(async move {
            match name {
                "echo" | "search" | "show" => Ok(Set::text(format!("{name}:{}", a.pos.join(" ")))),
                "ids" => Ok(Set::from_ids(a.pos.iter().map(|p| p.parse().unwrap()).collect())),
                "slow" => {
                    let ms: u64 = a.pos[0].parse().unwrap();
                    tokio::time::sleep(Duration::from_millis(ms)).await;
                    Ok(Set::text(format!("slow{ms}")))
                }
                "fail" => bail!("boom"),
                "rev" => {
                    let mut s = input.ok_or_else(|| anyhow!("no input"))?;
                    s.ids.reverse();
                    Ok(s)
                }
                "top" => {
                    let mut s = input.ok_or_else(|| anyhow!("no input"))?;
                    s.ids.truncate(a.pos[0].parse().unwrap());
                    Ok(s)
                }
                _ => bail!("unknown command '{name}'"),
            }
        })
    }

    fn fake_finish<'a>(_: &'a Ctx, set: Set, _a: &'a Args) -> LocalBoxFuture<'a, Result<String>> {
        Box::pin(async move {
            Ok(match set.text {
                Some(t) => t,
                None => set.ids.iter().map(u32::to_string).collect::<Vec<_>>().join(","),
            })
        })
    }

    const FAKE: Engine = Engine {
        dispatch: fake,
        finish: fake_finish,
        known: |n| matches!(n, "echo" | "search" | "show" | "ids" | "slow" | "fail" | "rev" | "top"),
    };

    fn args_finish<'a>(_: &'a Ctx, _set: Set, a: &'a Args) -> LocalBoxFuture<'a, Result<String>> {
        Box::pin(async move {
            Ok(format!("fmt={:?} abs={:?} f={:?} quiet={} nh={}", a.get("fmt"), a.get("abs"), a.get("fields"), a.flag("quiet"), a.flag("no-header")))
        })
    }

    #[test]
    fn render_options_carry_to_final_stage() {
        const ENG: Engine = Engine { dispatch: fake, finish: args_finish, known: FAKE.known };
        let run = |line: &str| tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(exec(ENG, &ctx(), lex_str(line).unwrap())).unwrap();
        assert_eq!(
            run("--fmt json --quiet --abs 25 ids 1 2 | top 1"),
            r#"fmt=Some("json") abs=Some("25") f=None quiet=true nh=false"#
        );
        assert_eq!(
            run("ids 1 2 --fmt json -f title | top 1 --fmt tsv"),
            r#"fmt=Some("tsv") abs=None f=Some("title") quiet=false nh=false"#
        );
        assert_eq!(run("ids 1 --no-header | top 1"), r#"fmt=None abs=None f=None quiet=false nh=true"#);
        assert_eq!(run("ids 1 --quiet | top 1 --quiet=false"), r#"fmt=None abs=None f=None quiet=false nh=false"#);
        assert_eq!(run("ids 1 --quiet | top 1 --quiet=false | top 1"), r#"fmt=None abs=None f=None quiet=false nh=false"#);
        assert_eq!(run("ids 1 --no-header | top 1 --no-header=false"), r#"fmt=None abs=None f=None quiet=false nh=false"#);
        assert_eq!(run("ids 1 --quiet=false | top 1 --quiet"), r#"fmt=None abs=None f=None quiet=true nh=false"#);
    }

    fn csv_finish<'a>(_: &'a Ctx, _set: Set, a: &'a Args) -> LocalBoxFuture<'a, Result<String>> {
        Box::pin(async move { Ok(format!("f={:?} abs={:?}", a.csv("fields"), a.get("abs"))) })
    }

    #[test]
    fn repeated_field_selections_all_carry() {
        const ENG: Engine = Engine { dispatch: fake, finish: csv_finish, known: FAKE.known };
        let run = |line: &str| tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(exec(ENG, &ctx(), lex_str(line).unwrap())).unwrap();
        assert_eq!(run("ids 1 -f pmid -f title | top 1"), r#"f=["pmid", "title"] abs=None"#);
        assert_eq!(run("ids 1 -f pmid,doi -f title --abs 5 --abs 9 | top 1"), r#"f=["pmid", "doi", "title"] abs=Some("9")"#);
        assert_eq!(run("ids 1 -f pmid -f title | top 1 -f doi"), r#"f=["doi"] abs=None"#);
    }

    #[test]
    fn later_stage_render_options_win() {
        const ENG: Engine = Engine { dispatch: fake, finish: args_finish, known: FAKE.known };
        let run = |line: &str| tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(exec(ENG, &ctx(), lex_str(line).unwrap())).unwrap();
        assert_eq!(
            run("ids 1 2 3 --fmt json --abs 10 | top 2 --fmt ids --abs 20 | top 1"),
            r#"fmt=Some("ids") abs=Some("20") f=None quiet=false nh=false"#
        );
        assert_eq!(run("ids 1 --abs 5 --abs 9 | top 1"), r#"fmt=None abs=Some("9") f=None quiet=false nh=false"#);
        assert_eq!(run("ids 1 2 --json | top 1"), r#"fmt=Some("json") abs=None f=None quiet=false nh=false"#);
        assert_eq!(
            run("ids 1 2 --json | top 1 --fmt tsv | top 1"),
            r#"fmt=Some("tsv") abs=None f=None quiet=false nh=false"#
        );
    }

    #[test]
    fn auto_fmt_with_json_flag_carries_json() {
        const ENG: Engine = Engine { dispatch: fake, finish: args_finish, known: FAKE.known };
        let run = |line: &str| tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(exec(ENG, &ctx(), lex_str(line).unwrap())).unwrap();
        assert_eq!(run("ids 1 2 --fmt auto --json | top 1"), r#"fmt=Some("json") abs=None f=None quiet=false nh=false"#);
        assert_eq!(run("ids 1 2 --json --fmt auto | top 1"), r#"fmt=Some("json") abs=None f=None quiet=false nh=false"#);
        assert_eq!(run("ids 1 2 --fmt json | top 1 --fmt auto"), r#"fmt=Some("auto") abs=None f=None quiet=false nh=false"#);
        assert_eq!(run("ids 1 2 --fmt tsv | top 1 --fmt auto --json"), r#"fmt=Some("auto") abs=None f=None quiet=false nh=false"#);
    }

    fn ctx() -> std::sync::Arc<Ctx> {
        Ctx::new(Config::default()).unwrap()
    }

    async fn go(line: &str) -> Result<String> {
        exec(FAKE, &ctx(), lex_str(line)?).await
    }

    #[test]
    fn lexer_respects_quotes() {
        assert_eq!(
            lex_str(r#"search "a | b; c" -n 5 | top 3"#).unwrap(),
            vec![w("search"), w("a | b; c"), w("-n"), w("5"), Tok::Pipe, w("top"), w("3")]
        );
        assert_eq!(lex_str("search 'x|y' ; show 1").unwrap(), vec![w("search"), w("x|y"), Tok::Semi, w("show"), w("1")]);
    }

    #[test]
    fn lexer_attached_operators_escapes_and_empties() {
        assert_eq!(lex_str("a|b;c").unwrap(), vec![w("a"), Tok::Pipe, w("b"), Tok::Semi, w("c")]);
        assert_eq!(lex_str(r"a\|b").unwrap(), vec![w("a|b")]);
        assert_eq!(lex_str(r#"a "" b"#).unwrap(), vec![w("a"), w(""), w("b")]);
        assert_eq!(lex_str(r#""say \"hi\" | now""#).unwrap(), vec![w(r#"say "hi" | now"#)]);
        assert_eq!(lex_str("  ").unwrap(), vec![]);
        assert_eq!(lex_str("a || b").unwrap(), vec![w("a"), Tok::Pipe, Tok::Pipe, w("b")]);
        assert!(lex_str("search \"oops").is_err());
    }

    #[test]
    fn argv_single_string_detection() {
        let one = |s: &str| lex(vec![s.to_string()]).unwrap();
        assert_eq!(one("search crispr -n 5 | top 3").len(), 7);
        assert_eq!(one("crispr cas9 | x"), vec![w("crispr cas9 | x")]);
        assert_eq!(one("search"), vec![w("search")]);
        assert_eq!(
            lex(vec!["search".into(), "a|b".into(), "|".into(), "top".into(), ";".into()]).unwrap(),
            vec![w("search"), w("a|b"), Tok::Pipe, w("top"), Tok::Semi]
        );
    }

    #[test]
    fn stage_parsing() {
        let st = |s: &str| parse_task(&FAKE, lex_str(s).unwrap());
        let names = |s: &str| st(s).unwrap().into_iter().map(|s| s.name).collect::<Vec<_>>();
        assert_eq!(names("crispr cas9 -n 5"), ["search"]);
        assert_eq!(names("-n 5 crispr"), ["search"]);
        assert_eq!(names("123 456"), ["show"]);
        assert_eq!(names("doi:10.1/x"), ["show"]);
        assert_eq!(names("123 abc"), ["search"]);
        assert_eq!(names("ids 1 | rev | top 1"), ["ids", "rev", "top"]);
        assert_eq!(st("crispr -n 5").unwrap()[0].args.get("limit"), Some("5"));
        assert!(st("echo a | nope").unwrap_err().to_string().contains("unknown command 'nope'"));
        assert!(st("echo a |").unwrap_err().to_string().contains("empty pipeline stage"));
        assert!(st("serch").unwrap_err().to_string().contains("did you mean 'search'"));
        assert_eq!(names("crispr"), ["search"]);
    }

    #[test]
    fn leading_globals_do_not_hide_command() {
        let names = |s: &str| parse_task(&FAKE, lex_str(s).unwrap()).unwrap().into_iter().map(|s| s.name).collect::<Vec<_>>();
        assert_eq!(names("--quiet echo x"), ["echo"]);
        assert_eq!(names("--no-cache --fmt tsv echo x"), ["echo"]);
        assert_eq!(names("--key=K --abs 5 echo x"), ["echo"]);
        assert_eq!(names("--limit 5 echo x"), ["search"]);
        let st = parse_task(&FAKE, lex_str("--quiet echo x").unwrap()).unwrap();
        assert!(st[0].args.flag("quiet"));
        assert_eq!(st[0].args.pos, ["x"]);
        assert_eq!(names("--limit 5 crispr"), ["search"]);
    }

    #[test]
    fn option_values_never_become_commands() {
        let stages = |argv: &[&str]| {
            let toks = lex(argv.iter().map(|s| s.to_string()).collect()).unwrap();
            parse_task(&PROD, toks).unwrap()
        };
        for argv in [["--ti", "cache memory clear"], ["--ti", "batch effects"], ["--ab", "journal club"], ["--tiab", "cache clear"]] {
            let st = stages(&argv);
            assert_eq!(st.len(), 1);
            assert_eq!(st[0].name, "search", "{argv:?}");
            assert_eq!(st[0].args.get(argv[0].trim_start_matches('-')), Some(argv[1]));
        }
        let toks = lex(vec!["-n".into(), "3".into(), "journal a ++ journal b".into()]).unwrap();
        assert_eq!(split_on(toks, &Tok::Semi).len(), 1);
        let st = stages(&["--quiet", "cache stats"]);
        assert_eq!(st[0].name, "cache");
        let st = stages(&["--limit", "5", "journal nature"]);
        assert_eq!(st[0].name, "search");
    }

    #[test]
    fn leading_options_skip_empty_tasks() {
        let toks = lex(vec!["--quiet".into(), "journal nature ;".into()]).unwrap();
        assert_eq!(task_text(&toks), "journal --quiet nature");
        let toks = lex(vec!["--quiet".into(), "journal a ; ; journal b".into()]).unwrap();
        assert_eq!(task_text(&toks), "journal --quiet a ; journal --quiet b");
    }

    #[test]
    fn double_dash_stops_command_hoisting() {
        let st = parse_task(&FAKE, lex_str("-- show 1").unwrap()).unwrap();
        assert_eq!(st[0].name, "search");
        assert_eq!(st[0].args.pos, ["show", "1"]);
        let st = parse_task(&FAKE, lex_str("--quiet -- echo x").unwrap()).unwrap();
        assert_eq!(st[0].name, "search");
        assert_eq!(st[0].args.pos, ["echo", "x"]);
    }

    #[test]
    fn secrets_are_redacted_in_headers() {
        let t = |s: &str| task_text(&lex_str(s).unwrap());
        assert_eq!(t("count x --key SECRET ; count y"), "count x --key *** ; count y");
        assert_eq!(t("count x --key=SECRET"), "count x --key=***");
        assert_eq!(t("count x --email a@b.c --quiet"), "count x --email *** --quiet");
        assert_eq!(redact("count x --key SECRET -n 3"), "count x --key *** -n 3");
        assert_eq!(redact("count x --email=a@b.c"), "count x --email=***");
        assert!(!redact("search --key SECRET").contains("SECRET"));
        assert_eq!(redact("help \"--key=SECRET\""), "help --key=***");
        assert_eq!(redact("help --key\tSECRET -n 3"), "help --key *** -n 3");
        assert_eq!(redact("help 'x"), "<unparsable>");
    }

    #[test]
    fn unparsable_stage_keys_never_echo() {
        let e = cmd::unknown("--key=REVIEW_DEMO_SECRET");
        assert!(!err_line(&e).contains("REVIEW_DEMO_SECRET"));
        assert_eq!(err_line(&e), "error: unknown command (pm help cmds)");
        let e = anyhow::anyhow!("odd '--key=REVIEW_DEMO_SECRET' and --email x@y.z");
        let m = err_line(&e);
        assert!(!m.contains("REVIEW_DEMO_SECRET") && !m.contains("x@y.z"), "{m}");
    }

    #[test]
    fn secrets_scrubbed_from_error_lines() {
        let e = anyhow::anyhow!("http 400: {{\"api-key\":\"TOPSECRET\"}}");
        assert!(!err_line(&e).contains("TOPSECRET"));
        assert!(err_line(&e).contains("***"));
    }

    #[test]
    fn bare_scope_flags_forwarded_from_search_stage() {
        let st = |name: &str, line: &str| Stage { name: name.into(), args: Args::parse(&line.split_whitespace().map(String::from).collect::<Vec<_>>()).unwrap() };
        let mut fin = Args::default();
        carry_query_terms(&mut fin, &st("search", "sepsis --ti -f snip"));
        assert!(fin.flags.iter().any(|f| f == "ti") && !fin.flags.iter().any(|f| f == "ab"));
        let mut fin = Args::default();
        carry_query_terms(&mut fin, &st("show", "1 --ti -f snip"));
        assert!(fin.flags.is_empty());
        let mut fin = Args::default();
        carry_query_terms(&mut fin, &st("s", "x --ab --tiab -f snip"));
        carry_query_terms(&mut fin, &st("search", "x --ab -f snip"));
        assert_eq!(fin.flags, ["ab", "tiab"]);
    }

    #[test]
    fn upstream_query_forwarded_for_snippets() {
        let search = Stage { name: "search".into(), args: Args::parse(&["statin OR dementia".to_string()]).unwrap() };
        let mut fin = Args::default();
        carry_query_terms(&mut fin, &search);
        assert_eq!(fin.all("query"), ["statin OR dementia"]);
        assert!(fin.all("any").is_empty());
    }

    #[test]
    fn clip_and_task_text() {
        assert_eq!(clip("abcdef", 3), "abc…");
        assert_eq!(clip("abc", 3), "abc");
        assert_eq!(clip("héllo wörld", 5), "héllo…");
        assert_eq!(task_text(&lex_str(r#"search "a b" | top 3"#).unwrap()), r#"search "a b" | top 3"#);
    }

    #[test]
    fn arts_loading_rule() {
        let f = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert!(wants_arts(&[], None));
        assert!(wants_arts(&f(&["pmid", "title"]), Some("tsv")));
        assert!(!wants_arts(&f(&["pmid"]), None));
        assert!(!wants_arts(&[], Some("ids")));
        assert!(!wants_arts(&f(&["title"]), Some("ids")));
    }

    #[tokio::test]
    async fn pipes_move_sets_between_stages() {
        assert_eq!(go("ids 1 2 3 | rev | top 2").await.unwrap(), "3,2");
        assert_eq!(go("ids 5 | rev").await.unwrap(), "5");
    }

    #[tokio::test]
    async fn implicit_search_runs() {
        assert_eq!(go("crispr cas9 -n 5").await.unwrap(), "search:crispr cas9");
        assert_eq!(go("123").await.unwrap(), "show:123");
    }

    #[tokio::test]
    async fn single_task_has_no_header_and_prefixes_stage_errors() {
        assert_eq!(go("echo hi").await.unwrap(), "echo:hi");
        let e = go("ids 1 | fail").await.unwrap_err();
        assert_eq!(err_line(&e), "error: fail: boom");
        let e = go("fail").await.unwrap_err();
        assert_eq!(err_line(&e), "error: boom");
    }

    #[tokio::test]
    async fn tasks_run_in_parallel_and_keep_order() {
        let t = Instant::now();
        let out = go("slow 400 ; slow 40 ; slow 240").await.unwrap();
        assert!(t.elapsed() < Duration::from_millis(600), "{:?}", t.elapsed());
        assert_eq!(out, "## slow 400\nslow400\n## slow 40\nslow40\n## slow 240\nslow240");
    }

    #[tokio::test]
    async fn failing_task_is_isolated() {
        let out = go("echo a ; fail ; ids 1 2 | rev").await.unwrap();
        assert_eq!(out, "## echo a\necho:a\n## fail\nerror: boom\n## ids 1 2 | rev\n2,1");
        assert!(failed());
    }

    #[tokio::test]
    async fn parse_error_in_one_task_is_isolated() {
        let out = go("echo a ; echo b | nope ; echo c").await.unwrap();
        assert_eq!(out, "## echo a\necho:a\n## echo b | nope\nerror: unknown command 'nope' (pm help cmds)\n## echo c\necho:c");
    }

    #[tokio::test]
    async fn header_is_clipped_and_empty_tasks_ignored() {
        let long = "x".repeat(80);
        let out = go(&format!("echo {long} ; ; echo b ;")).await.unwrap();
        let first = out.lines().next().unwrap();
        assert_eq!(first, format!("## echo {}…", "x".repeat(55)));
        assert_eq!(out.matches("## ").count(), 2);
        assert_eq!(go(" ; ; ").await.unwrap(), "");
    }

    #[tokio::test]
    async fn single_task_among_empties_has_no_header() {
        assert_eq!(go("; echo a ;").await.unwrap(), "echo:a");
    }
}
