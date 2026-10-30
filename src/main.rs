mod args;
mod cmd;
mod enrich;
mod eutils;
mod fields;
mod filter;
mod fuzzy;
mod help;
mod http;
mod journals;
mod model;
mod ops;
mod pipeline;
mod render;
mod xml;

use args::Args;
use http::{Config, Ctx};
use pipeline::Tok;
use std::io::Write;
use std::process::ExitCode;

fn early(toks: &[Tok]) -> Option<String> {
    let Some(first) = toks.first() else { return Some(help::text(None)) };
    let Tok::W(w) = first else { return None };
    if toks.iter().any(|t| !matches!(t, Tok::W(_))) {
        return None;
    }
    match w.as_str() {
        "-h" | "--help" | "help" => {
            let topic = match toks.get(1) {
                Some(Tok::W(t)) if !t.starts_with('-') => Some(t.as_str()),
                _ => None,
            };
            Some(http::redact(&help::text(topic)))
        }
        "--version" | "-V" => Some(format!("pm {}", env!("CARGO_PKG_VERSION"))),
        _ => None,
    }
}

fn write_all(w: &mut impl Write, s: &str) -> std::io::Result<()> {
    w.write_all(s.as_bytes())?;
    if !s.ends_with('\n') {
        w.write_all(b"\n")?;
    }
    w.flush()
}

fn emit(s: &str) -> bool {
    if s.is_empty() {
        return true;
    }
    let mut out = std::io::BufWriter::new(std::io::stdout().lock());
    match write_all(&mut out, s) {
        Ok(()) => true,
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => true,
        Err(e) => {
            eprintln!("error: write failed: {e}");
            false
        }
    }
}

fn scan_credentials(words: &[String], g: &mut Args) {
    let mut it = words.iter().peekable();
    while let Some(w) = it.next() {
        let Some((name, val)) = pipeline::secret_flag(w) else { continue };
        let value = match val {
            Some(v) => Some(v.to_string()),
            None => it.peek().filter(|n| !n.starts_with('-')).map(|n| n.to_string()),
        };
        if let Some(v) = value {
            g.opts.push((name.to_ascii_lowercase(), v));
        }
    }
}

fn globals(toks: &[Tok]) -> Args {
    let mut g = Args::default();
    for task in pipeline::split_on(toks.to_vec(), &Tok::Semi) {
        for stage in pipeline::split_on(task, &Tok::Pipe) {
            let words = pipeline::words(&stage);
            match Args::parse(&words) {
                Ok(a) => {
                    g.opts.extend(a.opts);
                    g.flags.extend(a.flags);
                }
                Err(_) => scan_credentials(&words, &mut g),
            }
        }
    }
    g
}

fn run(toks: Vec<Tok>) -> anyhow::Result<String> {
    let g = globals(&toks);
    let mut cfg = Config::from_env();
    cfg.no_cache = g.flag("no-cache");
    cfg.refresh = g.flag("refresh");
    cfg.quiet = g.flag("quiet");
    if let Some(k) = g.get("key") {
        cfg.api_key = Some(k.to_string());
    }
    if let Some(e) = g.get("email") {
        cfg.email = Some(e.to_string());
    }
    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(4).enable_all().build()?;
    let out = rt.block_on(async {
        let ctx = Ctx::new(cfg)?;
        pipeline::run_toks(&ctx, toks).await
    });
    rt.shutdown_background();
    out
}

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args_os().skip(1).map(|s| s.to_string_lossy().into_owned()).collect();
    let toks = match pipeline::lex(argv) {
        Ok(t) => t,
        Err(e) => return fail(&e),
    };
    if let Some(text) = early(&toks) {
        return if emit(&text) { ExitCode::SUCCESS } else { ExitCode::from(1) };
    }
    match run(toks) {
        Ok(out) => {
            let wrote = emit(&out);
            if pipeline::failed() || !wrote { ExitCode::from(1) } else { ExitCode::SUCCESS }
        }
        Err(e) => fail(&e),
    }
}

fn fail(e: &anyhow::Error) -> ExitCode {
    eprintln!("{}", pipeline::err_line(e));
    ExitCode::from(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Broken(std::io::ErrorKind);

    impl Write for Broken {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(self.0.into())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Err(self.0.into())
        }
    }

    #[test]
    fn credentials_register_even_when_a_stage_fails_to_parse() {
        let toks = pipeline::lex_str("top 1 11 --fmt ids | --key=REVIEW_DEMO_SECRET show --limt 1").unwrap();
        let g = globals(&toks);
        assert_eq!(g.get("key"), Some("REVIEW_DEMO_SECRET"));
        let toks = pipeline::lex_str("show 1 ; show --limt 1 --key SECRET2 --email a@b.c").unwrap();
        let g = globals(&toks);
        assert_eq!((g.get("key"), g.get("email")), (Some("SECRET2"), Some("a@b.c")));
    }

    #[test]
    fn option_like_help_topics_are_ignored() {
        let toks = pipeline::lex_str("help --key=SECRET123").unwrap();
        let text = early(&toks).unwrap();
        assert!(!text.contains("SECRET123"));
        assert_eq!(text, help::text(None));
        for t in ["help zzznotatopic", "help 'search --key=0123456789abcdef'"] {
            let text = early(&pipeline::lex_str(t).unwrap()).unwrap();
            assert!(text.starts_with("unknown topic\n") && text.contains("topics:"), "{text}");
            assert!(!text.contains("zzznotatopic") && !text.contains("0123456789abcdef"), "{text}");
        }
    }

    #[test]
    fn write_errors_surface() {
        assert_eq!(write_all(&mut Broken(std::io::ErrorKind::StorageFull), "x").unwrap_err().kind(), std::io::ErrorKind::StorageFull);
        let mut ok = Vec::new();
        write_all(&mut ok, "a").unwrap();
        assert_eq!(ok, b"a\n");
    }

    #[test]
    fn one_malformed_task_does_not_hide_globals() {
        let toks = pipeline::lex_str("journal nature --no-cache ; show -n").unwrap();
        let g = globals(&toks);
        assert!(g.flag("no-cache"));
        let toks = pipeline::lex_str("count x --key K | top 1 ; show --year").unwrap();
        assert_eq!(globals(&toks).get("key"), Some("K"));
    }
}
