use crate::args::Args;
use crate::fields::{self, Field, Kwic, Opts, push_clean, write_json_field, write_json_str, write_value};
use crate::model::*;
use anyhow::{Result, bail};
use std::fmt::Write;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Fmt {
    Tsv,
    Txt,
    Jsonl,
    Json,
    Ids,
}

struct Body {
    buf: String,
    ends: Vec<usize>,
    start: usize,
}

struct Meta<'a> {
    total: Option<u64>,
    skip: usize,
    notes: &'a [String],
    header: bool,
}

fn tokens(bytes: usize) -> usize {
    bytes.div_ceil(4)
}

pub fn render(set: &Set, a: &Args) -> Result<String> {
    let fmt = parse_fmt(a)?;
    let budget = a.num::<usize>("budget")?.filter(|&b| b > 0);
    if let Some(t) = &set.text {
        return Ok(text(t, budget));
    }
    let skip = match a.num("skip")? {
        Some(s) => s,
        None => a.num("offset")?.unwrap_or(0),
    };
    let mut meta = Meta { total: set.total, skip, notes: &set.notes, header: !a.flag("quiet") };
    if let Some(t) = &set.table {
        let explicit = matches!(fmt, Some(Fmt::Tsv | Fmt::Txt));
        let fmt = fmt.map_or(Fmt::Tsv, |f| if f == Fmt::Txt { Fmt::Tsv } else { f });
        meta.header &= fmt != Fmt::Json;
        return Ok(table(t, fmt, &meta, budget, a.flag("no-header"), explicit));
    }
    if set.arts.is_empty() && set.ids.is_empty() && matches!(fmt, Some(Fmt::Tsv | Fmt::Txt)) {
        return articles(set, a, fmt, meta, budget);
    }
    if set.arts.is_empty() && matches!(fmt, Some(Fmt::Tsv | Fmt::Txt)) {
        let t = Table { header: vec!["pmid".into()], rows: set.ids.iter().map(|i| vec![i.to_string()]).collect() };
        return Ok(table(&t, Fmt::Tsv, &meta, budget, a.flag("no-header"), true));
    }
    if set.arts.is_empty() || fmt == Some(Fmt::Ids) {
        let fmt = match fmt {
            Some(f @ (Fmt::Json | Fmt::Jsonl)) => f,
            _ => Fmt::Ids,
        };
        meta.header &= fmt != Fmt::Json;
        return Ok(ids(&set.ids, fmt, &meta, budget));
    }
    articles(set, a, fmt, meta, budget)
}

fn parse_fmt(a: &Args) -> Result<Option<Fmt>> {
    Ok(match a.get("fmt") {
        None | Some("auto") => a.flag("json").then_some(Fmt::Json),
        Some("tsv") => Some(Fmt::Tsv),
        Some("txt") => Some(Fmt::Txt),
        Some("jsonl") => Some(Fmt::Jsonl),
        Some("json") => Some(Fmt::Json),
        Some("ids") => Some(Fmt::Ids),
        Some(_) => bail!("--fmt: auto|tsv|txt|jsonl|json|ids"),
    })
}

fn text(t: &str, budget: Option<usize>) -> String {
    let t = t.trim_end_matches('\n');
    match budget {
        Some(b) if tokens(t.len()) > b => {
            let mut end = (b * 4).min(t.len());
            while !t.is_char_boundary(end) {
                end -= 1;
            }
            let end = t[..end].rfind('\n').unwrap_or(end);
            format!("{}\n#budget_cut", &t[..end])
        }
        _ => t.to_string(),
    }
}

fn build(n: usize, fmt: Fmt, prefix: &str, cap: usize, mut rec: impl FnMut(&mut String, usize)) -> Body {
    let mut buf = String::with_capacity(cap);
    buf.push_str(prefix);
    let start = buf.len();
    let mut ends = Vec::with_capacity(n);
    let sep = match fmt {
        Fmt::Txt => "\n\n",
        Fmt::Json => ",\n",
        _ => "\n",
    };
    for i in 0..n {
        if i > 0 {
            buf.push_str(sep);
        } else if start > 0 {
            buf.push('\n');
        }
        rec(&mut buf, i);
        ends.push(buf.len());
    }
    Body { buf, ends, start }
}

fn finish(body: &Body, fmt: Fmt, meta: &Meta, budget: Option<usize>) -> String {
    let n = body.ends.len();
    let closer = |shown: usize| match (fmt, shown) {
        (Fmt::Json, 0) => "]",
        (Fmt::Json, _) => "\n]",
        _ => "",
    };
    let head = |shown: usize| -> String {
        let mut h = String::new();
        if !meta.header {
            return h;
        }
        h.push('#');
        if let Some(t) = meta.total {
            let _ = write!(h, "total={t} ");
        }
        let _ = write!(h, "shown={shown} skip={}", meta.skip);
        for note in meta.notes {
            h.push(' ');
            h.push_str(note);
        }
        if shown < n {
            let _ = write!(h, " budget_cut={}", n - shown);
        }
        h.push('\n');
        h
    };
    let upto = |k: usize| if k == 0 { body.start } else { body.ends[k - 1] };
    let mut shown = n;
    if let Some(b) = budget {
        while shown > 1 && tokens(head(shown).len() + upto(shown) + closer(shown).len()) > b {
            shown -= 1;
        }
    }
    let h = head(shown);
    let mut out = String::with_capacity(h.len() + upto(shown) + 2);
    out.push_str(&h);
    out.push_str(&body.buf[..upto(shown)]);
    out.push_str(closer(shown));
    out
}

fn shrink(cap: usize) -> Option<usize> {
    match cap {
        0 => Some(1000),
        c if c > 80 => Some((c / 2).max(80)),
        _ => None,
    }
}

fn articles(set: &Set, a: &Args, fmt: Option<Fmt>, mut meta: Meta, budget: Option<usize>) -> Result<String> {
    let (mut fl, defaulted) = fields::choose(set, a)?;
    if defaulted && set.arts.iter().all(|x| x.jr.is_none()) {
        fl.retain(|f| *f != Field::Q);
    }
    let fmt = fmt.unwrap_or(if fl.iter().any(|f| f.is_long()) { Fmt::Txt } else { Fmt::Tsv });
    meta.header &= fmt != Fmt::Json;
    let prefix = match fmt {
        Fmt::Json => "[".to_string(),
        Fmt::Tsv if !a.flag("no-header") => fl.iter().map(|f| f.name()).collect::<Vec<_>>().join("\t"),
        _ => String::new(),
    };
    let mut o = Opts::new(set, a)?;
    if fl.contains(&Field::Snip) {
        o.snip = Some(Kwic::from_args(a)?);
    }
    let shrinkable = fl.iter().any(|f| f.is_abstract());
    let mut scratch = String::new();
    loop {
        let cap = set.arts.len() * (80 + 60 * fl.len());
        let body = build(set.arts.len(), fmt, &prefix, cap, |buf, i| {
            let art = &set.arts[i];
            match fmt {
                Fmt::Tsv => {
                    for (j, f) in fl.iter().enumerate() {
                        if j > 0 {
                            buf.push('\t');
                        }
                        write_value(buf, art, *f, &o);
                    }
                }
                Fmt::Txt => txt_record(buf, art, &fl, &o),
                _ => {
                    buf.push('{');
                    for f in &fl {
                        write_json_field(buf, art, *f, &o, &mut scratch);
                    }
                    buf.push('}');
                }
            }
        });
        if let (Some(b), true) = (budget, shrinkable)
            && tokens(body.buf.len() + 40) > b
            && let Some(c) = shrink(o.abs)
        {
            o.abs = c;
            continue;
        }
        return Ok(finish(&body, fmt, &meta, budget));
    }
}

fn seg(buf: &mut String, pre: &[&str], f: impl FnOnce(&mut String)) {
    let mark = buf.len();
    for p in pre {
        buf.push_str(p);
    }
    let mid = buf.len();
    f(buf);
    if buf.len() == mid {
        buf.truncate(mark);
    }
}

fn txt_record(buf: &mut String, a: &Article, fl: &[Field], o: &Opts) {
    let _ = write!(buf, "{}", a.pmid);
    seg(buf, &[" | "], |b| {
        let mid = b.len();
        for f in [Field::Year, Field::Journal, Field::Q] {
            if !fl.contains(&f) {
                continue;
            }
            seg(b, if b.len() > mid { &[" "][..] } else { &[][..] }, |b| {
                let at = b.len();
                write_value(b, a, f, o);
                if &b[at..] == "-" {
                    b.truncate(at);
                }
            });
        }
    });
    if fl.contains(&Field::Authors) {
        seg(buf, &[" | "], |b| write_value(b, a, Field::Authors, o));
    }
    if fl.contains(&Field::Title) {
        seg(buf, &["\n"], |b| write_value(b, a, Field::Title, o));
    }
    for f in fl {
        if !matches!(f, Field::Pmid | Field::Year | Field::Journal | Field::Q | Field::Authors | Field::Title) {
            seg(buf, &["\n", f.name(), ": "], |b| write_value(b, a, *f, o));
        }
    }
}

fn ids(ids: &[u32], fmt: Fmt, meta: &Meta, budget: Option<usize>) -> String {
    let prefix = if fmt == Fmt::Json { "[" } else { "" };
    let body = build(ids.len(), fmt, prefix, ids.len() * 9, |buf, i| {
        let _ = if matches!(fmt, Fmt::Json | Fmt::Jsonl) {
            write!(buf, "{{\"pmid\":{}}}", ids[i])
        } else {
            write!(buf, "{}", ids[i])
        };
    });
    finish(&body, fmt, meta, budget)
}

const STRING_COLS: &[&str] = &["issn", "eissn", "doi", "pmc", "orcid", "ui", "tree", "version"];

fn json_number(s: &str) -> bool {
    let d = s.strip_prefix('-').unwrap_or(s);
    let (int, frac) = d.split_once('.').map_or((d, None), |(i, f)| (i, Some(f)));
    !int.is_empty()
        && int.bytes().all(|b| b.is_ascii_digit())
        && (int == "0" || !int.starts_with('0'))
        && frac.is_none_or(|f| !f.is_empty() && f.bytes().all(|b| b.is_ascii_digit()))
        && int.len() + frac.map_or(0, str::len) <= 15
}

fn table(t: &Table, fmt: Fmt, meta: &Meta, budget: Option<usize>, no_header: bool, header_when_empty: bool) -> String {
    let mut prefix = String::new();
    if matches!(fmt, Fmt::Json) {
        prefix.push('[');
    } else if fmt == Fmt::Tsv && !no_header && (header_when_empty || !t.rows.is_empty()) {
        for (j, h) in t.header.iter().enumerate() {
            if j > 0 {
                prefix.push('\t');
            }
            push_clean(&mut prefix, h);
        }
    }
    let cap = t.rows.len() * 80;
    let numeric: Vec<bool> = (0..t.header.len())
        .map(|j| !STRING_COLS.contains(&t.header[j].as_str()) && t.rows.iter().filter_map(|r| r.get(j)).all(|c| c.trim().is_empty() || json_number(c.trim())))
        .collect();
    let body = build(t.rows.len(), fmt, &prefix, cap, |buf, i| {
        let row = &t.rows[i];
        match fmt {
            Fmt::Tsv | Fmt::Txt => {
                for (j, c) in row.iter().enumerate() {
                    if j > 0 {
                        buf.push('\t');
                    }
                    push_clean(buf, c);
                }
            }
            Fmt::Ids => push_clean(buf, row.first().map_or("", String::as_str)),
            _ => {
                buf.push('{');
                let mut scratch = String::new();
                for ((h, c), num) in t.header.iter().zip(row).zip(&numeric) {
                    scratch.clear();
                    push_clean(&mut scratch, c);
                    if scratch.is_empty() {
                        continue;
                    }
                    if !buf.ends_with('{') {
                        buf.push(',');
                    }
                    write_json_str(buf, h);
                    buf.push(':');
                    if *num {
                        buf.push_str(&scratch);
                    } else {
                        write_json_str(buf, &scratch);
                    }
                }
                buf.push('}');
            }
        }
    });
    finish(&body, fmt, meta, budget)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &str) -> Args {
        Args::parse(&crate::args::split_words(s).unwrap()).unwrap()
    }

    fn art(pmid: u32, title: &str) -> Article {
        Article {
            pmid,
            title: title.into(),
            abs: vec![
                ("BACKGROUND".into(), "alpha ".repeat(40)),
                ("CONCLUSIONS".into(), "Final words.".into()),
            ],
            authors: ["Smith J", "Lee K", "Wu H", "Zed Z"]
                .iter()
                .map(|n| Author { name: n.to_string(), ..Default::default() })
                .collect(),
            journal: JournalInfo { iso: "Lancet".into(), title: "The Lancet".into(), ..Default::default() },
            year: 2021,
            ..Default::default()
        }
    }

    fn set(n: u32) -> Set {
        let mut s = Set::from_arts((1..=n).map(|i| art(i, "T\tx")).collect());
        s.total = Some(1234);
        s
    }

    #[test]
    fn explicit_tsv_on_id_only_set_keeps_header() {
        let s = Set::from_ids(vec![7, 8]);
        assert_eq!(render(&s, &args("--quiet --fmt tsv")).unwrap(), "pmid\n7\n8");
        assert_eq!(render(&s, &args("--quiet")).unwrap(), "7\n8");
        assert_eq!(render(&s, &args("--quiet --fmt ids")).unwrap(), "7\n8");
    }

    #[test]
    fn empty_result_keeps_tsv_schema() {
        let s = Set::default();
        let out = render(&s, &args("--fmt tsv -f pmid,title")).unwrap();
        assert_eq!(out, "#shown=0 skip=0\npmid\ttitle");
        assert_eq!(render(&s, &args("--fmt tsv --no-header -f pmid,title")).unwrap(), "#shown=0 skip=0\n");
        assert_eq!(render(&s, &args("")).unwrap(), "#shown=0 skip=0\n");
    }

    #[test]
    fn empty_table_keeps_header_only_when_tsv_is_explicit() {
        let t = Set::table(&["pmid", "doi", "pmc"], Vec::new());
        assert_eq!(render(&t, &args("--fmt tsv")).unwrap(), "#shown=0 skip=0\npmid\tdoi\tpmc");
        assert_eq!(render(&t, &args("--fmt tsv --no-header")).unwrap(), "#shown=0 skip=0\n");
        assert_eq!(render(&t, &args("")).unwrap(), "#shown=0 skip=0\n");
    }

    #[test]
    fn tsv_default() {
        let out = render(&set(2), &args("")).unwrap();
        assert_eq!(
            out,
            "#total=1234 shown=2 skip=0\npmid\tyear\tjournal\tauthors\ttitle\n\
             1\t2021\tLancet\tSmith J, Lee K … Zed Z\tT x\n2\t2021\tLancet\tSmith J, Lee K … Zed Z\tT x"
        );
    }

    #[test]
    fn tsv_ranked_quiet_no_header() {
        let mut s = set(1);
        s.arts[0].jr = Some(JournalRank { q: 1, ..Default::default() });
        let out = render(&s, &args("--quiet --no-header --authors 1")).unwrap();
        assert_eq!(out, "1\t2021\tLancet\tQ1\tSmith J +3\tT x");
    }

    #[test]
    fn txt_blocks() {
        let mut s = set(2);
        s.arts[0].jr = Some(JournalRank { q: 2, ..Default::default() });
        let out = render(&s, &args("-f +concl,mesh --quiet")).unwrap();
        assert_eq!(
            out,
            "1 | 2021 Lancet Q2 | Smith J, Lee K … Zed Z\nT x\nconcl: Final words.\n\n\
             2 | 2021 Lancet | Smith J, Lee K … Zed Z\nT x\nconcl: Final words."
        );
    }

    #[test]
    fn auto_goes_txt_for_abs_and_caps() {
        let out = render(&set(2), &args("-f pmid,abs --abs 30 --quiet")).unwrap();
        assert_eq!(out, "1\nabs: BG: alpha alpha alpha alpha…\n\n2\nabs: BG: alpha alpha alpha alpha…");
    }

    #[test]
    fn full_note_uncaps_abstracts_for_every_record() {
        let long = "word ".repeat(400);
        let mut s = set(2);
        for x in &mut s.arts {
            x.abs = vec![("".into(), long.clone())];
        }
        let capped = render(&s, &args("-f pmid,abs --quiet")).unwrap();
        assert!(capped.contains('…') && capped.len() < 2 * long.len());
        s.notes.push("full".into());
        for fmt in ["txt", "tsv", "json", "jsonl"] {
            let out = render(&s, &args(&format!("-f pmid,abs --quiet --fmt {fmt}"))).unwrap();
            assert!(!out.contains('…'), "{fmt}");
            assert!(out.matches("word").count() >= 800, "{fmt}");
        }
    }

    #[test]
    fn single_record_abs_is_full() {
        let out = render(&set(1), &args("-f pmid,abs --quiet")).unwrap();
        assert!(out.ends_with("C: Final words."), "{out}");
    }

    #[test]
    fn jsonl_and_json() {
        let mut s = set(2);
        s.scores.insert(1, 0.5);
        s.arts[0].types = vec!["Randomized Controlled Trial".into()];
        let out = render(&s, &args("--fmt jsonl -f pmid,year,q,types,title,score")).unwrap();
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines[0], "#total=1234 shown=2 skip=0");
        assert_eq!(
            lines[1],
            r#"{"pmid":1,"year":2021,"types":["Randomized Controlled Trial"],"title":"T x","score":0.50}"#
        );
        assert_eq!(lines[2], r#"{"pmid":2,"year":2021,"title":"T x"}"#);
        let j = render(&s, &args("--fmt json -f pmid,title")).unwrap();
        let v: serde_json::Value = serde_json::from_str(&j).unwrap();
        assert_eq!(v[1]["pmid"], 2);
        assert_eq!(v.as_array().unwrap().len(), 2);
    }

    #[test]
    fn json_tables_keep_identifiers_as_strings_and_columns_consistent() {
        let rows = vec![
            vec!["00284793".to_string(), "Nature".into(), "19.713".into(), "1495".into(), "0123".into()],
            vec!["14764687".to_string(), "Cell".into(), "1.660".into(), "9".into(), "45".into()],
            vec!["25425641".to_string(), "X".into(), "".into(), "12".into(), "+5".into()],
        ];
        let s = Set::table(&["issn", "title", "sjr", "h", "code"], rows);
        let j = render(&s, &args("--fmt json")).unwrap();
        let v: serde_json::Value = serde_json::from_str(&j).unwrap();
        assert_eq!(v[0]["issn"], "00284793");
        assert_eq!(v[1]["issn"], "14764687");
        assert_eq!(v[0]["sjr"], 19.713);
        assert_eq!(v[1]["sjr"], 1.66);
        assert!(v[2]["sjr"].is_null());
        assert_eq!(v[2]["h"], 12);
        assert_eq!(v[0]["code"], "0123");
        assert_eq!(v[1]["code"], "45");
        assert!(["1e5", "+1", "01", "1.", ".5", "-", ""].iter().all(|x| !json_number(x)));
        assert!(["0", "-3", "10.50", "0.25"].iter().all(|x| json_number(x)));
    }

    #[test]
    fn ids_format_and_ids_only_sets() {
        assert_eq!(render(&set(3), &args("--fmt ids --quiet")).unwrap(), "1\n2\n3");
        let s = Set::from_ids(vec![5, 6]);
        assert_eq!(render(&s, &args("")).unwrap(), "#shown=2 skip=0\n5\n6");
        assert_eq!(render(&s, &args("--fmt json")).unwrap(), "[\n{\"pmid\":5},\n{\"pmid\":6}\n]");
        assert_eq!(render(&s, &args("--fmt jsonl --quiet")).unwrap(), "{\"pmid\":5}\n{\"pmid\":6}");
        assert_eq!(render(&Set::default(), &args("--fmt json")).unwrap(), "[]");
    }

    #[test]
    fn table_and_text() {
        let t = Set::table(&["journal", "n"], vec![vec!["A\tB".into(), "3".into()], vec!["C".into(), "".into()]]);
        assert_eq!(render(&t, &args("--quiet")).unwrap(), "journal\tn\nA B\t3\nC\t");
        assert_eq!(render(&t, &args("--quiet --no-header")).unwrap(), "A B\t3\nC\t");
        assert_eq!(render(&t, &args("--quiet --fmt jsonl")).unwrap(), "{\"journal\":\"A B\",\"n\":3}\n{\"journal\":\"C\"}");
        assert_eq!(render(&Set::text("x\ny\n\n"), &args("")).unwrap(), "x\ny");
    }

    #[test]
    fn notes_and_skip_in_header() {
        let mut s = set(1);
        s.notes = vec!["scanned=600".into()];
        let out = render(&s, &args("--skip 20 --fmt ids")).unwrap();
        assert_eq!(out, "#total=1234 shown=1 skip=20 scanned=600\n1");
    }

    #[test]
    fn budget_shrinks_abs_then_drops_records() {
        let s = set(20);
        let full = render(&s, &args("-f pmid,abs --abs 0 --quiet")).unwrap();
        let small = render(&s, &args("-f pmid,abs --abs 0 --budget 600")).unwrap();
        assert!(tokens(small.len()) <= 600, "{}", tokens(small.len()));
        assert!(small.len() < full.len());
        assert!(!small.contains("budget_cut"), "{small}");
        let tight = render(&s, &args("-f pmid,title --budget 30")).unwrap();
        assert!(tokens(tight.len()) <= 30);
        let head = tight.lines().next().unwrap();
        assert!(head.starts_with("#total=1234 shown=") && head.contains("budget_cut="), "{head}");
        let shown: usize = head.split("shown=").nth(1).unwrap().split(' ').next().unwrap().parse().unwrap();
        assert_eq!(tight.lines().count(), 1 + 1 + shown);
    }

    #[test]
    fn budget_keeps_one_record_and_json_stays_valid() {
        let s = set(5);
        let out = render(&s, &args("--fmt json --budget 5")).unwrap();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v.as_array().unwrap().len(), 1);
    }

    #[test]
    fn snip_field_tsv_from_render_args() {
        let mut s = set(1);
        s.arts[0].abs = vec![("RESULTS".into(), "x ".repeat(50) + "placebo arm worse. " + &"y ".repeat(50))];
        let out = render(&s, &args("-f pmid,y,snip --grep placebo --snip 10 --quiet")).unwrap();
        assert_eq!(out, "pmid\tyear\tsnip\n1\t2021\t…x x x x x placebo arm worse.…");
        let out = render(&s, &args("--grep placebo --snip 5 -f=+snip,-authors,-q --quiet --no-header")).unwrap();
        assert_eq!(out, "1\t2021\tLancet\tT x\t…x x placebo arm…");
    }

    #[test]
    fn bad_inputs() {
        assert!(render(&set(1), &args("--fmt xml")).unwrap_err().to_string().contains("--fmt"));
        assert!(render(&set(1), &args("-f titl")).unwrap_err().to_string().contains("did you mean 'title'"));
        assert!(render(&set(1), &args("-f snip --snip x")).unwrap_err().to_string().contains("--snip"));
    }

    #[test]
    fn sample_list_token_cost() {
        let out = render(&set(10), &args("")).unwrap();
        assert!(tokens(out.len()) < 200, "{}", tokens(out.len()));
    }
}
