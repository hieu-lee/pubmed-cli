use crate::args::Args;
use crate::http::Ctx;
use crate::model::*;
use anyhow::{Result, bail};
use std::fmt::Write;
use std::path::{Path, PathBuf};

fn wants_clear(pos: &[String]) -> Result<bool> {
    match pos.first().map(String::as_str) {
        None | Some("info" | "stats") => Ok(false),
        Some("clear") => Ok(true),
        Some(x) => bail!("cache: unknown action '{x}' (use: info|stats|clear)"),
    }
}

pub async fn run(ctx: &Ctx, a: &Args) -> Result<Set> {
    let clear = wants_clear(&a.pos)?;
    let dir = ctx.cfg().cache_dir.clone();
    let dry = a.flag("dry");
    let text = tokio::task::spawn_blocking(move || if clear { clear_dir(&dir, dry) } else { stats(&dir) }).await??;
    Ok(Set::text(text))
}

fn walk(p: &Path) -> (u64, u64) {
    let Ok(rd) = std::fs::read_dir(p) else { return (0, 0) };
    let (mut n, mut bytes) = (0, 0);
    for e in rd.flatten() {
        let Ok(m) = e.path().symlink_metadata() else { continue };
        if m.is_dir() {
            let (a, b) = walk(&e.path());
            n += a;
            bytes += b;
        } else {
            n += 1;
            bytes += m.len();
        }
    }
    (n, bytes)
}

pub fn fmt_size(b: u64) -> String {
    const U: [&str; 4] = ["KB", "MB", "GB", "TB"];
    if b < 1024 {
        return format!("{b}B");
    }
    let mut v = b as f64 / 1024.0;
    let mut u = 0;
    while v >= 1024.0 && u < U.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    format!("{v:.1}{}", U[u])
}

fn stats(dir: &Path) -> Result<String> {
    let (n, bytes) = walk(dir);
    let mut s = format!("dir {}\nentries {n} size {}", dir.display(), fmt_size(bytes));
    let mut ns: Vec<_> = std::fs::read_dir(dir).into_iter().flatten().flatten().collect();
    ns.sort_by_key(|e| e.file_name());
    for e in ns {
        let (n, b) = if e.path().symlink_metadata().is_ok_and(|m| m.is_dir()) { walk(&e.path()) } else { (1, 0) };
        let _ = write!(s, "\n{} {n} {}", e.file_name().to_string_lossy(), fmt_size(b));
    }
    Ok(s)
}

fn has_pm_name(dir: &Path) -> bool {
    dir.file_name().is_some_and(|n| n.to_string_lossy().to_ascii_lowercase().contains("pm"))
}

fn normalized(dir: &Path) -> PathBuf {
    dir.components().collect()
}

pub fn safe_to_clear(dir: &Path) -> bool {
    let dir = normalized(dir);
    let Ok(meta) = dir.symlink_metadata() else { return has_pm_name(&dir) };
    if meta.file_type().is_symlink() {
        return false;
    }
    let Ok(real) = dir.canonicalize() else { return false };
    let home = std::env::var_os("HOME").and_then(|h| PathBuf::from(h).canonicalize().ok());
    dir.file_name() == real.file_name()
        && has_pm_name(&real)
        && has_pm_name(&dir)
        && home.as_deref() != Some(real.as_path())
}

fn clear_dir(dir: &Path, dry: bool) -> Result<String> {
    let dir = normalized(dir);
    let dir = dir.as_path();
    if !safe_to_clear(dir) {
        bail!("cache: refusing to clear '{}' (must be a real directory named *pm*, not a symlink)", dir.display());
    }
    let rd = match std::fs::read_dir(dir) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok("cache empty".into()),
        r => r?,
    };
    let (n, bytes) = walk(dir);
    if dry {
        return Ok(format!("would clear {n} entries {}", fmt_size(bytes)));
    }
    for e in rd.flatten() {
        let p = e.path();
        if p.symlink_metadata()?.is_dir() {
            std::fs::remove_dir_all(&p)?;
        } else {
            std::fs::remove_file(&p)?;
        }
    }
    Ok(format!("cleared {n} entries {}", fmt_size(bytes)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("pm-cachetest-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("art/ab")).unwrap();
        std::fs::write(d.join("art/ab/f1"), vec![0u8; 2048]).unwrap();
        std::fs::write(d.join("art/f2"), b"x").unwrap();
        std::fs::create_dir_all(d.join("srch")).unwrap();
        std::fs::write(d.join("srch/f3"), b"yy").unwrap();
        d
    }

    #[test]
    fn actions() {
        let v = |s: &str| vec![s.to_string()];
        assert!(!wants_clear(&v("stats")).unwrap());
        assert!(!wants_clear(&[]).unwrap());
        assert!(wants_clear(&v("clear")).unwrap());
        assert!(wants_clear(&v("nuke")).is_err());
    }

    #[test]
    fn sizes() {
        assert_eq!(fmt_size(0), "0B");
        assert_eq!(fmt_size(1023), "1023B");
        assert_eq!(fmt_size(1536), "1.5KB");
        assert_eq!(fmt_size(5 * 1024 * 1024), "5.0MB");
    }

    #[test]
    fn stats_lists_namespaces() {
        let d = scratch("stats");
        let s = stats(&d).unwrap();
        let lines: Vec<&str> = s.lines().collect();
        assert_eq!(lines[1], "entries 3 size 2.0KB");
        assert_eq!(lines[2], "art 2 2.0KB");
        assert_eq!(lines[3], "srch 1 2B");
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn clear_removes_contents_keeps_dir() {
        let d = scratch("clear");
        assert_eq!(clear_dir(&d, false).unwrap(), "cleared 3 entries 2.0KB");
        assert!(d.is_dir());
        assert_eq!(std::fs::read_dir(&d).unwrap().count(), 0);
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn dry_run_removes_nothing() {
        let d = scratch("dry");
        assert_eq!(clear_dir(&d, true).unwrap(), "would clear 3 entries 2.0KB");
        assert_eq!(walk(&d).0, 3);
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn refuses_suspicious_paths() {
        assert!(!safe_to_clear(Path::new("/")));
        assert!(!safe_to_clear(Path::new("/home/someone/data")));
        assert!(clear_dir(Path::new("/tmp/stuff"), false).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn refuses_symlinked_cache_dir() {
        let base = std::env::temp_dir().join(format!("pm-symtest-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let victim = base.join("victim");
        std::fs::create_dir_all(&victim).unwrap();
        std::fs::write(victim.join("keep"), b"x").unwrap();
        let link = base.join("pm");
        std::os::unix::fs::symlink(&victim, &link).unwrap();
        assert!(!safe_to_clear(&link));
        assert!(clear_dir(&link, false).is_err());
        assert!(victim.join("keep").exists());
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn trailing_slash_does_not_bypass_symlink_guard() {
        let base = std::env::temp_dir().join(format!("pm-slashtest-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let victim = base.join("pm-data");
        std::fs::create_dir_all(&victim).unwrap();
        std::fs::write(victim.join("keep"), b"x").unwrap();
        let link = base.join("pm-link");
        std::os::unix::fs::symlink(&victim, &link).unwrap();
        for spelled in [format!("{}/", link.display()), format!("{}//", link.display()), format!("{}/.", link.display())] {
            let p = Path::new(&spelled);
            assert!(!safe_to_clear(p), "{spelled}");
            assert!(clear_dir(p, false).is_err(), "{spelled}");
        }
        assert!(victim.join("keep").exists());
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn symlink_to_pm_named_dir_is_refused() {
        let base = std::env::temp_dir().join(format!("pm-namedtest-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let victim = base.join("pm-data");
        std::fs::create_dir_all(&victim).unwrap();
        std::fs::write(victim.join("keep"), b"x").unwrap();
        let link = base.join("pm");
        std::os::unix::fs::symlink(&victim, &link).unwrap();
        assert!(!safe_to_clear(&link));
        assert!(clear_dir(&link, false).is_err());
        assert!(victim.join("keep").exists());
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn trailing_slash_on_real_dir_still_clears() {
        let d = scratch("slash");
        let spelled = format!("{}/", d.display());
        assert_eq!(clear_dir(Path::new(&spelled), true).unwrap(), "would clear 3 entries 2.0KB");
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn clear_does_not_follow_inner_symlinks() {
        let base = std::env::temp_dir().join(format!("pm-innertest-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let victim = base.join("victim");
        std::fs::create_dir_all(&victim).unwrap();
        std::fs::write(victim.join("keep"), b"x").unwrap();
        let dir = base.join("pm");
        std::fs::create_dir_all(&dir).unwrap();
        std::os::unix::fs::symlink(&victim, dir.join("art")).unwrap();
        clear_dir(&dir, false).unwrap();
        assert!(victim.join("keep").exists());
        assert!(!dir.join("art").exists());
        std::fs::remove_dir_all(&base).unwrap();
    }
}
