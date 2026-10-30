use crate::args::Args;
use crate::http::Ctx;
use crate::model::*;
use crate::pipeline;
use anyhow::{Result, bail};
use futures::StreamExt;
use std::io::{IsTerminal, Read};

const IN_FLIGHT: usize = 6;

pub fn pipeline_lines(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(String::from)
        .collect()
}

const HTTP_GLOBALS: &[&str] = &["no-cache", "refresh", "key", "email"];

fn stage_has_global(words: Vec<String>) -> bool {
    Args::parse(&words).is_ok_and(|a| HTTP_GLOBALS.iter().any(|g| a.has(g)))
}

fn check_line(line: &str) -> Result<()> {
    let mut stage: Vec<String> = Vec::new();
    let mut bad = false;
    for t in pipeline::lex_str(line)? {
        match t {
            pipeline::Tok::W(w) => stage.push(w),
            _ => bad |= stage_has_global(std::mem::take(&mut stage)),
        }
    }
    bad |= stage_has_global(stage);
    if bad {
        bail!("batch: --no-cache/--refresh/--key/--email belong on the pm command, not inside a line");
    }
    Ok(())
}

async fn read_input(a: &Args) -> Result<String> {
    if !a.pos.is_empty() {
        return Ok(a.pos.join("\n"));
    }
    if let Some(f) = a.get("file").or_else(|| a.get("fields")) {
        return tokio::fs::read_to_string(f).await.map_err(|e| anyhow::anyhow!("batch: {f}: {e}"));
    }
    if std::io::stdin().is_terminal() {
        bail!("batch: no input (give pipelines as args, -f FILE, or stdin)");
    }
    Ok(tokio::task::spawn_blocking(|| {
        let mut s = String::new();
        std::io::stdin().lock().read_to_string(&mut s).map(|_| s)
    })
    .await??)
}

pub async fn run(ctx: &Ctx, a: &Args) -> Result<Set> {
    let lines = pipeline_lines(&read_input(a).await?);
    if lines.is_empty() {
        bail!("batch: no pipelines");
    }
    let outs: Vec<String> = futures::stream::iter(lines.into_iter().enumerate())
        .map(|(i, line)| async move {
            let res = match check_line(&line) {
                Ok(()) => pipeline::run_str(ctx, &line).await,
                Err(e) => Err(e),
            };
            let body = match res {
                Ok(s) => s,
                Err(e) => {
                    pipeline::mark_failed();
                    pipeline::err_line(&e)
                }
            };
            format!("## {} {}\n{}", i + 1, pipeline::clip(&pipeline::redact(&line), 60), body.trim_end_matches('\n'))
        })
        .buffered(IN_FLIGHT)
        .collect()
        .await;
    Ok(Set::text(outs.join("\n")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn double_dash_in_one_stage_does_not_hide_globals_in_another() {
        for l in ["journal -- nature :: journal science --refresh", "journal -- a ++ search b --no-cache", "journal -- a | show 1 --key=K"] {
            assert!(check_line(l).is_err(), "{l}");
        }
        assert!(check_line("journal -- nature :: journal science").is_ok());
    }

    #[test]
    fn http_globals_rejected_inside_lines() {
        for l in ["search a --no-cache", "search a --key=K", "show 1 | top 1 --email x@y.z", "count x --refresh"] {
            assert!(check_line(l).unwrap_err().to_string().starts_with("batch: --no-cache"), "{l}");
        }
        assert!(check_line("search \"--no-cache\" -n 2").is_err());
        assert!(check_line("search a -n 2 --quiet").is_ok());
        for l in ["search a --no_cache", "search a -refresh", "search a -key K", "search a -email=x@y.z", "search a --key=K"] {
            assert!(check_line(l).is_err(), "{l}");
        }
    }

    #[test]
    fn skips_blanks_and_comments() {
        let t = "search a | top 2\n\n  # note\n  show 1 ; show 2  \n";
        assert_eq!(pipeline_lines(t), vec!["search a | top 2", "show 1 ; show 2"]);
    }
}
