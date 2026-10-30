use anyhow::{Result, bail};
use std::str::FromStr;

pub const BOOLS: &[&str] = &[
    "fuzzy", "published", "unpublished", "preprint", "no-retracted", "retracted", "free", "fulltext",
    "oa", "abstract", "data", "clinical", "has-comments", "has-problems", "no-cache", "refresh",
    "quiet", "expand", "explain", "tree", "list", "all-sections", "desc", "asc", "human", "animal", "exact", "by-year",
    "full", "json", "dry", "no-header", "scores", "unique", "count", "pmc", "review", "trial",
    "rct", "meta", "case", "guideline", "sys", "comments", "problems", "hits", "reverse", "help", "areas", "update", "other", "reviews",
];

pub const KNOWN_OPTS: &[&str] = &[
    "ab", "abs", "abstract", "aff", "age", "all", "all-sections", "animal", "any", "area", "areas", "asc",
    "author", "authors", "budget", "by", "by-year", "case", "cat", "chars", "cites-min", "clinical", "comments",
    "count", "country", "data", "datetype", "desc", "dry", "email", "exact", "expand", "explain", "fields",
    "file", "first-author", "fmt", "free", "from", "full", "fulltext", "fuzzy", "fz", "grant", "grep", "grep-ab",
    "grep-ti", "guideline", "has-comments", "has-problems", "help", "hindex-min", "hits", "human", "issn",
    "journal", "json", "key", "kw", "lang", "last", "last-author", "limit", "list", "major", "mesh", "meta",
    "min", "min-score", "nct", "near", "new", "no-cache", "no-grep", "no-header", "no-retracted", "not",
    "not-type", "oa", "offset", "other", "phrase", "pmc", "preprint", "problems", "published", "quartile",
    "query", "quiet", "rcr-min", "rct", "refresh", "retracted", "reverse", "review", "reviews", "scan", "scores",
    "sec", "sex", "since", "sjr-min", "skip", "snip", "snips", "sort", "species", "status", "style", "sys",
    "tag", "ti", "tiab", "to", "tree", "trial", "type", "unique", "unpublished", "until", "update", "where",
    "within", "year",
];

fn takes_optional(name: &str, next: &str) -> bool {
    if next.starts_with('-') {
        return false;
    }
    name != "nct" || next.get(..3).is_some_and(|p| p.eq_ignore_ascii_case("nct")) && next.len() > 3 && next[3..].bytes().all(|b| b.is_ascii_digit())
}

pub const OPTIONAL_VALUE: &[&str] = &["ti", "tiab", "ab", "nct"];

pub const ALIASES: &[(&str, &str)] = &[
    ("n", "limit"),
    ("f", "fields"),
    ("a", "author"),
    ("j", "journal"),
    ("y", "year"),
    ("t", "type"),
    ("s", "sort"),
    ("k", "kw"),
    ("m", "mesh"),
    ("i", "aff"),
    ("l", "lang"),
    ("o", "offset"),
    ("q", "quartile"),
    ("b", "budget"),
    ("w", "where"),
    ("h", "help"),
];

#[derive(Debug, Clone, Default)]
pub struct Args {
    pub pos: Vec<String>,
    pub opts: Vec<(String, String)>,
    pub flags: Vec<String>,
}

pub fn opt_arity(tok: &str) -> usize {
    let body = tok.trim_start_matches('-');
    if !tok.starts_with('-') || body.is_empty() || body.contains('=') || is_number(tok) {
        return 0;
    }
    let name = canon(body);
    usize::from(!BOOLS.contains(&name.as_str()) && !OPTIONAL_VALUE.contains(&name.as_str()))
}

const HOIST_BOOLS: &[&str] = &["quiet", "no-cache", "refresh", "no-header", "json", "scores"];
const HOIST_VALUED: &[&str] = &["key", "email", "fmt", "abs", "authors", "budget"];

/// Arity of a leading option that may precede the command word: Some(0|1) values still to consume, None when the option is not an allowed global.
pub fn global_opt(tok: &str) -> Option<usize> {
    if !tok.starts_with('-') || tok == "-" || tok == "--" {
        return None;
    }
    let body = tok.trim_start_matches('-');
    let (name, inline) = match body.split_once('=') {
        Some((n, _)) => (n, true),
        None => (body, false),
    };
    let name = canon(name);
    if HOIST_BOOLS.contains(&name.as_str()) {
        Some(0)
    } else if HOIST_VALUED.contains(&name.as_str()) {
        Some(usize::from(!inline))
    } else {
        None
    }
}

fn canon(name: &str) -> String {
    for (s, l) in ALIASES {
        if *s == name {
            return l.to_string();
        }
    }
    name.replace('_', "-")
}

impl Args {
    pub fn parse(tokens: &[String]) -> Result<Args> {
        let mut a = Args::default();
        let mut i = 0;
        let mut only_pos = false;
        while i < tokens.len() {
            let t = &tokens[i];
            i += 1;
            if only_pos || t == "-" || !t.starts_with('-') || is_number(t) {
                a.pos.push(t.clone());
                continue;
            }
            if t == "--" {
                only_pos = true;
                continue;
            }
            let body = t.trim_start_matches('-');
            if body.is_empty() {
                bail!("bad option {t}");
            }
            let (name, inline) = match body.split_once('=') {
                Some((n, v)) => (canon(n), Some(v.to_string())),
                None => (canon(body), None),
            };
            if !KNOWN_OPTS.contains(&name.as_str()) {
                bail!("{}", unknown_option(&name));
            }
            if let Some(v) = inline {
                if BOOLS.contains(&name.as_str()) {
                    match v.to_ascii_lowercase().as_str() {
                        "1" | "true" | "yes" | "y" => a.set_bool(name, true),
                        "0" | "false" | "no" | "n" => a.set_bool(name, false),
                        _ => bail!("--{name}: bad boolean '{v}'"),
                    }
                } else {
                    a.opts.push((name, v));
                }
            } else if BOOLS.contains(&name.as_str()) {
                a.set_bool(name, true);
            } else if OPTIONAL_VALUE.contains(&name.as_str()) {
                if i < tokens.len() && takes_optional(&name, &tokens[i]) {
                    a.opts.push((name, tokens[i].clone()));
                    i += 1;
                } else {
                    a.flags.push(name);
                }
            } else if i < tokens.len() && (!tokens[i].starts_with('-') || is_number(&tokens[i]) || tokens[i] == "-") {
                a.opts.push((name, tokens[i].clone()));
                i += 1;
            } else if name == "fields" {
                bail!("-f needs a value; remove from default with -f=-q,-authors");
            } else {
                bail!("--{name} needs a value");
            }
        }
        Ok(a)
    }

    fn set_bool(&mut self, name: String, on: bool) {
        self.flags.retain(|f| *f != name);
        self.opts.retain(|(n, v)| !(*n == name && v == "false"));
        if on {
            self.flags.push(name);
        } else {
            self.opts.push((name, "false".into()));
        }
    }

    pub fn get(&self, k: &str) -> Option<&str> {
        self.opts.iter().rev().find(|(n, _)| n == k).map(|(_, v)| v.as_str())
    }

    pub fn all(&self, k: &str) -> Vec<&str> {
        self.opts.iter().filter(|(n, _)| n == k).map(|(_, v)| v.as_str()).collect()
    }

    pub fn flag(&self, k: &str) -> bool {
        self.flags.iter().any(|f| f == k)
            || self.get(k).is_some_and(|v| matches!(v.to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "y"))
    }

    pub fn has(&self, k: &str) -> bool {
        let off = BOOLS.contains(&k);
        self.flag(k) || self.opts.iter().any(|(n, v)| n == k && !(off && v == "false"))
    }

    pub fn explicit(&self, k: &str) -> bool {
        self.flags.iter().any(|f| f == k) || self.opts.iter().any(|(n, _)| n == k)
    }

    pub fn num<T: FromStr>(&self, k: &str) -> Result<Option<T>> {
        match self.get(k) {
            None => Ok(None),
            Some(v) => match v.parse::<T>() {
                Ok(x) => Ok(Some(x)),
                Err(_) => bail!("--{k}: bad number '{v}'"),
            },
        }
    }

    pub fn csv(&self, k: &str) -> Vec<String> {
        self.all(k)
            .into_iter()
            .flat_map(|v| v.split(',').map(|s| s.trim().to_string()))
            .filter(|s| !s.is_empty())
            .collect()
    }

    pub fn first_pos_or_all(&self) -> String {
        self.pos.join(" ")
    }
}

fn unknown_option(name: &str) -> String {
    let best = KNOWN_OPTS
        .iter()
        .map(|k| (crate::cmd::lev(name, k), *k))
        .filter(|(d, _)| *d <= if name.len() >= 6 { 2 } else { 1 })
        .min_by_key(|(d, _)| *d);
    match best {
        Some((_, k)) => format!("unknown option --{name} (did you mean --{k}?)"),
        None => format!("unknown option --{name} (pm help filters)"),
    }
}

fn is_number(s: &str) -> bool {
    s.strip_prefix('-').is_some_and(|r| !r.is_empty() && r.chars().all(|c| c.is_ascii_digit() || c == '.'))
}

#[cfg(test)]
pub fn split_words(line: &str) -> Result<Vec<String>> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut have = false;
    let mut q: Option<char> = None;
    let mut it = line.chars().peekable();
    while let Some(c) = it.next() {
        match q {
            Some(qc) => {
                if c == qc {
                    q = None;
                } else if c == '\\' && qc == '"' && matches!(it.peek(), Some('"') | Some('\\')) {
                    cur.push(it.next().unwrap());
                } else {
                    cur.push(c);
                }
            }
            None => match c {
                '"' | '\'' => {
                    q = Some(c);
                    have = true;
                }
                '\\' => {
                    if let Some(n) = it.next() {
                        cur.push(n);
                        have = true;
                    }
                }
                c if c.is_whitespace() => {
                    if have {
                        out.push(std::mem::take(&mut cur));
                        have = false;
                    }
                }
                c => {
                    cur.push(c);
                    have = true;
                }
            },
        }
    }
    if q.is_some() {
        bail!("unterminated quote");
    }
    if have {
        out.push(cur);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    #[test]
    fn inline_booleans_are_case_insensitive_and_validated() {
        let p = |s: &str| Args::parse(&crate::args::split_words(s).unwrap());
        assert!(p("--dry=True").unwrap().flag("dry"));
        assert!(p("--dry=YES").unwrap().flag("dry"));
        assert!(p("--fuzzy=1").unwrap().flag("fuzzy"));
        let off = p("--dry=False").unwrap();
        assert!(!off.flag("dry") && !off.has("dry") && off.explicit("dry"));
        assert!(!Args::parse(&[]).unwrap().explicit("dry"));
        assert_eq!(p("--dry=maybe").unwrap_err().to_string(), "--dry: bad boolean 'maybe'");
    }

    use super::*;

    #[test]
    fn leading_render_booleans_are_global() {
        for t in ["--json", "--scores", "--quiet", "--no-cache", "--refresh", "--no-header"] {
            assert_eq!(global_opt(t), Some(0), "{t}");
        }
        assert_eq!(global_opt("--fmt"), Some(1));
        assert_eq!(global_opt("--fuzzy"), None);
        assert_eq!(global_opt("--limit"), None);
    }

    fn parse(line: &str) -> Result<Args> {
        Args::parse(&split_words(line).unwrap())
    }

    #[test]
    fn missing_value_errors() {
        assert_eq!(parse("cancer --year").unwrap_err().to_string(), "--year needs a value");
        assert_eq!(parse("cancer -y --quiet").unwrap_err().to_string(), "--year needs a value");
        assert_eq!(parse("cancer -n").unwrap_err().to_string(), "--limit needs a value");
        assert!(parse("cancer -f").unwrap_err().to_string().starts_with("-f needs a value"));
        assert!(parse("cancer -f -q").unwrap_err().to_string().starts_with("-f needs a value"));
        assert!(parse("cancer --year 2020").is_ok());
        assert!(parse("cancer --year=2020").is_ok());
        assert!(parse("cancer --since -5").is_ok());
    }

    #[test]
    fn optional_value_options_work_bare() {
        for line in ["cancer --ti", "cancer --tiab -t rct", "cancer --ab", "cancer --nct"] {
            assert!(parse(line).is_ok(), "{line}");
        }
        let a = parse("cancer --tiab -t rct").unwrap();
        assert!(a.flag("tiab"));
        assert_eq!(a.get("type"), Some("rct"));
    }

    #[test]
    fn nct_takes_only_nct_ids() {
        let a = parse("--nct \"heart failure\" --ab").unwrap();
        assert!(a.flag("nct") && a.flag("ab"));
        assert_eq!(a.pos, ["heart failure"]);
        assert_eq!(parse("--nct NCT01234567").unwrap().get("nct"), Some("NCT01234567"));
        assert_eq!(parse("--nct nct01234567").unwrap().get("nct"), Some("nct01234567"));
        assert!(parse("--nct NCTX").unwrap().flag("nct"));
        let a = parse("--ti -t rct cancer").unwrap();
        assert!(a.flag("ti"));
        assert_eq!(a.get("type"), Some("rct"));
        assert_eq!(a.pos, ["cancer"]);
    }

    #[test]
    fn arity() {
        assert_eq!(opt_arity("--quiet"), 0);
        assert_eq!(opt_arity("--limit"), 1);
        assert_eq!(opt_arity("-n"), 1);
        assert_eq!(opt_arity("--limit=5"), 0);
        assert_eq!(opt_arity("--ti"), 0);
        assert_eq!(opt_arity("crispr"), 0);
        assert_eq!(opt_arity("-5"), 0);
    }

    #[test]
    fn unknown_options_are_rejected() {
        assert_eq!(parse("cancer --limt 5").unwrap_err().to_string(), "unknown option --limt (did you mean --limit?)");
        assert!(parse("cancer --n3").unwrap_err().to_string().starts_with("unknown option --n3"));
        assert!(parse("cache clear --dry-run=true").unwrap_err().to_string().starts_with("unknown option --dry-run"));
        assert!(parse("cancer --zzzzzzzz").unwrap_err().to_string().ends_with("(pm help filters)"));
        assert!(parse("cancer --help").is_ok());
        assert!(parse("cancer -- --limt").is_ok());
        assert!(parse("cancer --no_cache").is_ok());
    }

    #[test]
    fn registry_covers_declared_options() {
        for b in BOOLS.iter().chain(OPTIONAL_VALUE).chain(ALIASES.iter().map(|(_, l)| l)) {
            assert!(KNOWN_OPTS.contains(b), "{b}");
        }
        let mut sorted = KNOWN_OPTS.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted, KNOWN_OPTS, "KNOWN_OPTS must stay sorted and unique");
    }

    fn rs_files(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                rs_files(&p, out);
            } else if p.extension().is_some_and(|x| x == "rs") {
                out.push(p);
            }
        }
    }

    #[test]
    fn registry_covers_option_literals_in_source() {
        let getter = regex::Regex::new(
            r#"(?:\.(?:get|all|flag|has|csv|num)(?:::<[^>]*>)?\(\s*|opts\.push\(\(\s*|\(\s*(?:a|args|fin|b)\s*,\s*&?)"([a-z][a-z0-9-]*)""#,
        )
        .unwrap();
        let ignore = ["retry-after", "animals", "humans", "citedby", "metrics", "refs", "related"];
        let mut files = Vec::new();
        rs_files(&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), &mut files);
        let mut missing = Vec::new();
        for f in files {
            let src = std::fs::read_to_string(&f).unwrap();
            for c in getter.captures_iter(&src) {
                let name = &c[1];
                if !KNOWN_OPTS.contains(&name) && !ignore.contains(&name) {
                    missing.push(format!("{name} ({})", f.display()));
                }
            }
        }
        assert!(missing.is_empty(), "options used in code but missing from KNOWN_OPTS: {missing:?}");
    }

    #[test]
    fn boolean_flags_are_last_wins() {
        let p = |s: &str| Args::parse(&crate::args::split_words(s).unwrap()).unwrap();
        assert!(!p("--fuzzy --fuzzy=false").flag("fuzzy"));
        assert!(!p("--json --json=false").flag("json"));
        assert!(p("--json=false --json").flag("json"));
        assert!(p("--json=false --json=true").flag("json"));
        assert!(!p("--json --json=false --json=0").flag("json"));
        assert!(p("--json --json=false --json").flag("json"));
        let off = p("--json --json=false");
        assert!(off.explicit("json") && !off.has("json"));
        let on = p("--json=false --json");
        assert!(on.explicit("json") && on.has("json"));
        assert!(!p("--quiet=false").flag("quiet"));
        assert!(p("show 1 --json=false --json").flag("json"));
    }
}
