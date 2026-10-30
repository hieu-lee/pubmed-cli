use super::show::loaded;
use crate::args::Args;
use crate::http::Ctx;
use crate::model::*;
use anyhow::{Result, bail};

#[derive(Clone, Copy, PartialEq)]
enum Style {
    Vancouver,
    Nlm,
    Ama,
    Apa,
    Bibtex,
    Ris,
}

fn split_name(n: &str) -> (&str, &str) {
    match n.rsplit_once(' ') {
        Some((l, i)) if i.len() <= 4 && i.chars().all(|c| c.is_uppercase()) => (l, i),
        _ => (n, ""),
    }
}

fn dotted(initials: &str) -> String {
    initials.chars().flat_map(|c| [c, '.', ' ']).collect::<String>().trim_end().to_string()
}

fn limited(auth: &[Author], keep_all: usize, shown: usize) -> (&[Author], bool) {
    if auth.len() > keep_all { (&auth[..shown], true) } else { (auth, false) }
}

fn pages_range(p: &str) -> (&str, Option<String>) {
    match p.split_once('-') {
        Some((s, e)) if !e.is_empty() => {
            let ep = if e.len() < s.len() && e.bytes().all(|b| b.is_ascii_digit()) && s.bytes().all(|b| b.is_ascii_digit()) {
                format!("{}{e}", &s[..s.len() - e.len()])
            } else {
                e.to_string()
            };
            (s, Some(ep))
        }
        _ => (p, None),
    }
}

fn title_of(a: &Article) -> &str {
    a.title.trim().trim_end_matches('.')
}

fn volume(a: &Article) -> String {
    let j = &a.journal;
    let mut s = j.vol.clone();
    if !j.issue.is_empty() {
        s.push_str(&format!("({})", j.issue));
    }
    s
}

fn vancouver(a: &Article, st: Style) -> String {
    let (keep, shown) = match st {
        Style::Nlm => (10, 10),
        Style::Ama => (6, 3),
        _ => (6, 6),
    };
    let (auth, more) = limited(&a.authors, keep, shown);
    let mut names: Vec<String> = auth
        .iter()
        .map(|x| {
            let (l, i) = split_name(&x.name);
            if i.is_empty() { l.to_string() } else { format!("{l} {i}") }
        })
        .collect();
    if more {
        names.push("et al".into());
    }
    let mut s = String::new();
    if !names.is_empty() {
        s.push_str(&names.join(", "));
        s.push_str(". ");
    }
    s.push_str(title_of(a));
    s.push('.');
    let j = &a.journal;
    let jn = if j.iso.is_empty() { &j.title } else { &j.iso };
    if !jn.is_empty() {
        s.push_str(&format!(" {jn}."));
    }
    if a.year > 0 {
        s.push_str(&format!(" {}", a.year));
    }
    let vi = volume(a);
    if !vi.is_empty() {
        s.push_str(&format!(";{vi}"));
    }
    if !j.pages.is_empty() {
        s.push_str(&format!("{}{}", if vi.is_empty() { ";" } else { ":" }, j.pages));
    }
    s.push('.');
    if !a.doi.is_empty() {
        s.push_str(&format!(" doi:{}", a.doi));
    }
    s
}

fn apa(a: &Article) -> String {
    let n = a.authors.len();
    let fmt = |x: &Author| {
        let (l, i) = split_name(&x.name);
        if i.is_empty() { l.to_string() } else { format!("{l}, {}", dotted(i)) }
    };
    let names: Vec<String> = if n > 20 {
        let mut v: Vec<String> = a.authors[..19].iter().map(fmt).collect();
        v.push(format!("... {}", fmt(&a.authors[n - 1])));
        v
    } else {
        a.authors.iter().map(fmt).collect()
    };
    let mut s = match names.len() {
        0 => String::new(),
        1 => names[0].clone(),
        _ if n > 20 => names.join(", "),
        2 => format!("{} & {}", names[0], names[1]),
        k => format!("{}, & {}", names[..k - 1].join(", "), names[k - 1]),
    };
    if !s.is_empty() && !s.ends_with('.') {
        s.push('.');
    }
    if !s.is_empty() {
        s.push(' ');
    }
    if a.year > 0 {
        s.push_str(&format!("({}). ", a.year));
    }
    s.push_str(title_of(a));
    s.push('.');
    let j = &a.journal;
    let jn = if j.title.is_empty() { &j.iso } else { &j.title };
    if !jn.is_empty() {
        s.push_str(&format!(" {jn}"));
        if !j.vol.is_empty() {
            s.push_str(&format!(", {}", j.vol));
            if !j.issue.is_empty() {
                s.push_str(&format!("({})", j.issue));
            }
        }
        if !j.pages.is_empty() {
            let (sp, ep) = pages_range(&j.pages);
            s.push_str(&format!(", {sp}"));
            if let Some(e) = ep {
                s.push_str(&format!("\u{2013}{e}"));
            }
        }
        s.push('.');
    }
    if !a.doi.is_empty() {
        s.push_str(&format!(" https://doi.org/{}", a.doi));
    }
    s
}

fn tex(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(c, '&' | '%' | '_' | '#' | '$') {
            o.push('\\');
        }
        o.push(c);
    }
    o
}

fn bib_key(a: &Article) -> String {
    let last = a.authors.first().map(|x| split_name(&x.name).0).unwrap_or("anon");
    let word = a
        .title
        .split(|c: char| !c.is_alphanumeric())
        .find(|w| w.len() > 3 && !matches!(w.to_ascii_lowercase().as_str(), "this" | "that" | "with" | "from" | "does" | "what"))
        .unwrap_or("");
    let k: String = format!("{last}{}{word}", a.year).chars().filter(char::is_ascii_alphanumeric).collect();
    k.to_lowercase()
}

fn bibtex(a: &Article) -> String {
    let authors: Vec<String> = a
        .authors
        .iter()
        .map(|x| {
            let (l, i) = split_name(&x.name);
            let given = if x.fore.is_empty() { i } else { &x.fore };
            if given.is_empty() { format!("{{{l}}}") } else { format!("{l}, {given}") }
        })
        .collect();
    let j = &a.journal;
    let mut s = format!("@article{{{},\n", bib_key(a));
    let mut put = |k: &str, v: String| {
        if !v.is_empty() {
            s.push_str(&format!("  {k} = {{{v}}},\n"));
        }
    };
    put("author", authors.join(" and "));
    put("title", format!("{{{}}}", tex(title_of(a))));
    put("journal", tex(if j.iso.is_empty() { &j.title } else { &j.iso }));
    put("year", if a.year > 0 { a.year.to_string() } else { String::new() });
    put("volume", j.vol.clone());
    put("number", j.issue.clone());
    put("pages", match pages_range(&j.pages) {
        (sp, Some(ep)) => format!("{sp}--{ep}"),
        (sp, None) => sp.to_string(),
    });
    put("doi", a.doi.clone());
    put("pmid", a.pmid.to_string());
    s.pop();
    s.pop();
    s.push_str("\n}");
    s
}

fn ris(a: &Article) -> String {
    let j = &a.journal;
    let mut s = String::from("TY  - JOUR\n");
    let mut put = |k: &str, v: &str| {
        if !v.is_empty() {
            s.push_str(&format!("{k}  - {v}\n"));
        }
    };
    for x in &a.authors {
        let (l, i) = split_name(&x.name);
        put("AU", &if i.is_empty() { l.to_string() } else { format!("{l}, {i}") });
    }
    put("TI", title_of(a));
    put("JO", &j.title);
    put("JA", &j.iso);
    put("PY", &if a.year > 0 { a.year.to_string() } else { String::new() });
    put("VL", &j.vol);
    put("IS", &j.issue);
    let (sp, ep) = pages_range(&j.pages);
    put("SP", sp);
    put("EP", ep.as_deref().unwrap_or(""));
    put("DO", &a.doi);
    put("AN", &a.pmid.to_string());
    s.push_str("ER  - ");
    s
}

fn parse_style(s: &str) -> Result<Style> {
    Ok(match s.to_ascii_lowercase().as_str() {
        "vancouver" | "v" => Style::Vancouver,
        "nlm" => Style::Nlm,
        "ama" => Style::Ama,
        "apa" => Style::Apa,
        "bibtex" | "bib" => Style::Bibtex,
        "ris" => Style::Ris,
        _ => bail!("cite: style must be vancouver|ama|apa|bibtex|ris|nlm"),
    })
}

fn format_one(a: &Article, st: Style) -> String {
    match st {
        Style::Vancouver | Style::Nlm | Style::Ama => vancouver(a, st),
        Style::Apa => apa(a),
        Style::Bibtex => bibtex(a),
        Style::Ris => ris(a),
    }
}

const STYLE_WORDS: [&str; 6] = ["vancouver", "ama", "apa", "bibtex", "ris", "nlm"];

fn take_style_word(a: &Args) -> (Option<String>, Args) {
    let mut b = a.clone();
    let at = b.pos.iter().position(|p| STYLE_WORDS.contains(&p.to_ascii_lowercase().as_str()));
    (at.map(|i| b.pos.remove(i)), b)
}

pub async fn run(ctx: &Ctx, a: &Args, input: Option<Set>) -> Result<Set> {
    let (word, a) = take_style_word(a);
    let st = parse_style(a.get("style").or(word.as_deref()).unwrap_or("vancouver"))?;
    let set = loaded(ctx, &a, input).await?;
    let sep = if matches!(st, Style::Bibtex | Style::Ris) { "\n\n" } else { "\n" };
    let out: Vec<String> = set.arts.iter().map(|x| format_one(x, st)).collect();
    Ok(Set::text(out.join(sep)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn au(n: &str, f: &str) -> Author {
        Author { name: n.into(), fore: f.into(), ..Default::default() }
    }

    fn art() -> Article {
        Article {
            pmid: 123,
            title: "Deep learning for X & Y.".into(),
            authors: vec![au("Smith JA", "John A"), au("Van Der Berg B", "Bart"), au("Roe C", "Carl")],
            journal: JournalInfo {
                title: "Nature Medicine".into(),
                iso: "Nat Med".into(),
                vol: "12".into(),
                issue: "3".into(),
                pages: "345-52".into(),
                ..Default::default()
            },
            year: 2019,
            doi: "10.1/x".into(),
            ..Default::default()
        }
    }

    #[test]
    fn vancouver_basic() {
        assert_eq!(
            format_one(&art(), Style::Vancouver),
            "Smith JA, Van Der Berg B, Roe C. Deep learning for X & Y. Nat Med. 2019;12(3):345-52. doi:10.1/x"
        );
    }

    #[test]
    fn et_al_rules() {
        let mut a = art();
        a.authors = (0..8).map(|i| au(&format!("Name{i} A"), "")).collect();
        let v = format_one(&a, Style::Vancouver);
        assert!(v.starts_with("Name0 A, Name1 A, Name2 A, Name3 A, Name4 A, Name5 A, et al. "), "{v}");
        let m = format_one(&a, Style::Ama);
        assert!(m.starts_with("Name0 A, Name1 A, Name2 A, et al. "), "{m}");
        let n = format_one(&a, Style::Nlm);
        assert!(n.contains("Name7 A. Deep"), "{n}");
    }

    #[test]
    fn apa_basic() {
        assert_eq!(
            format_one(&art(), Style::Apa),
            "Smith, J. A., Van Der Berg, B., & Roe, C. (2019). Deep learning for X & Y. Nature Medicine, 12(3), 345\u{2013}352. https://doi.org/10.1/x"
        );
    }

    #[test]
    fn collective_author() {
        let mut a = art();
        a.authors = vec![au("ESC Working Group", "")];
        assert!(format_one(&a, Style::Vancouver).starts_with("ESC Working Group. Deep"));
    }

    #[test]
    fn bibtex_and_ris() {
        let b = format_one(&art(), Style::Bibtex);
        assert!(b.starts_with("@article{smith2019deep,\n  author = {Smith, John A and Van Der Berg, Bart and Roe, Carl},"), "{b}");
        assert!(b.contains("title = {{Deep learning for X \\& Y}},"));
        assert!(b.contains("pages = {345--352},"));
        assert!(b.ends_with("pmid = {123}\n}"));
        let r = format_one(&art(), Style::Ris);
        assert!(r.starts_with("TY  - JOUR\nAU  - Smith, JA\n"));
        assert!(r.contains("SP  - 345\nEP  - 352\n"));
        assert!(r.ends_with("ER  - "));
    }

    #[test]
    fn positional_style_word() {
        let a = Args::parse(&["bibtex".to_string(), "123".to_string()]).unwrap();
        let (w, b) = take_style_word(&a);
        assert_eq!(w.as_deref(), Some("bibtex"));
        assert_eq!(b.pos, ["123"]);
        let (w, b) = take_style_word(&Args::parse(&["123".to_string()]).unwrap());
        assert_eq!((w, b.pos), (None, vec!["123".to_string()]));
    }

    #[test]
    fn style_names() {
        assert!(parse_style("APA").is_ok());
        assert!(parse_style("mla").is_err());
    }
}
