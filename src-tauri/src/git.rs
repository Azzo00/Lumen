//! "Git pull" tile: for every repository directly inside the projects folder
//! (settings → gitRoot), four at a time: switch to `main` (or `master`) if another
//! branch is checked out, then `git pull --rebase`. Repositories that are not safe to
//! touch automatically are skipped and listed: local uncommitted changes, no
//! main/master, rebase/merge already in progress. A failed rebase is aborted so the
//! repository is left as it was. Details go to git-pull.log.
//!
//! Hovering the tile shows the status without pulling: `git fetch` of main/master
//! only (updates origin/<main>, never the working tree), then how many commits the
//! local main/master is behind or ahead. Cached for 2 minutes.

use std::collections::VecDeque;
use std::io::Read;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};

static RUNNING: AtomicBool = AtomicBool::new(false);
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const TIMEOUT: Duration = Duration::from_secs(180);
const WORKERS: usize = 4;

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct Progress {
    done: usize,
    total: usize,
}

#[derive(Serialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
struct Summary {
    total: usize,
    updated: Vec<String>,
    up_to_date: usize,
    skipped: Vec<(String, String)>,
    failed: Vec<(String, String)>,
    error: Option<String>,
}

enum Outcome {
    Updated(String),
    /// note: branch switch done before pulling, if any
    UpToDate(String),
    Skipped(String),
    Failed(String, String),
}

/// Returns false if a run is already in progress.
#[tauri::command]
pub fn git_pull_all(app: AppHandle) -> bool {
    if RUNNING.swap(true, Ordering::SeqCst) {
        return false;
    }
    std::thread::spawn(move || {
        let summary = run(&app);
        if let Ok(mut c) = STATUS.lock() {
            *c = None;
        }
        let _ = app.emit("git-done", summary);
        RUNNING.store(false, Ordering::SeqCst);
    });
    true
}

#[tauri::command]
pub fn open_git_log(app: AppHandle) {
    if let Some(p) = log_path(&app).filter(|p| p.exists()) {
        crate::launch::shell_open(&p.to_string_lossy());
    }
}

fn log_path(app: &AppHandle) -> Option<PathBuf> {
    app.path().app_config_dir().ok().map(|d| d.join("git-pull.log"))
}

fn run(app: &AppHandle) -> Summary {
    let root = PathBuf::from(crate::settings::get().git_root.trim());
    let mut repos: Vec<PathBuf> = match std::fs::read_dir(&root) {
        Ok(rd) => rd.flatten().map(|e| e.path()).filter(|p| p.join(".git").exists()).collect(),
        Err(_) => {
            return Summary { error: Some(format!("Cartella non trovata: {}", root.display())), ..Default::default() };
        }
    };
    repos.sort_by_key(|p| p.file_name().map(|n| n.to_ascii_lowercase()));
    let total = repos.len();
    if total == 0 {
        return Summary { error: Some(format!("Nessun repository in {}", root.display())), ..Default::default() };
    }
    let _ = app.emit("git-progress", Progress { done: 0, total });

    let queue = Arc::new(Mutex::new(repos.into_iter().collect::<VecDeque<_>>()));
    let results: Arc<Mutex<Vec<(String, Outcome, String)>>> = Arc::new(Mutex::new(Vec::new()));
    let workers: Vec<_> = (0..WORKERS.min(total))
        .map(|_| {
            let (queue, results, app) = (queue.clone(), results.clone(), app.clone());
            std::thread::spawn(move || loop {
                let Some(repo) = queue.lock().ok().and_then(|mut q| q.pop_front()) else { break };
                let name = repo.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                let mut log = String::new();
                let outcome = pull_one(&repo, &mut log);
                let done = results.lock().map(|mut r| {
                    r.push((name, outcome, log));
                    r.len()
                });
                if let Ok(done) = done {
                    let _ = app.emit("git-progress", Progress { done, total });
                }
            })
        })
        .collect();
    for w in workers {
        let _ = w.join();
    }

    let mut results = std::mem::take(&mut *results.lock().unwrap());
    results.sort_by_key(|(n, _, _)| n.to_lowercase());
    let mut summary = Summary { total, ..Default::default() };
    let mut log = format!("Lumen · git pull --rebase · {}\r\nCartella: {}\r\n\r\n", chrono::Local::now().format("%d/%m/%Y %H:%M:%S"), root.display());
    for (name, outcome, detail) in results {
        let line = match &outcome {
            Outcome::Updated(what) => format!("[AGGIORNATO]   {name}: {what}"),
            Outcome::UpToDate(note) if note.is_empty() => format!("[GIÀ AGGIORNATO] {name}"),
            Outcome::UpToDate(note) => format!("[GIÀ AGGIORNATO] {name}: {note}"),
            Outcome::Skipped(why) => format!("[SALTATO]      {name}: {why}"),
            Outcome::Failed(why, _) => format!("[ERRORE]       {name}: {why}"),
        };
        log.push_str(&line);
        log.push_str("\r\n");
        if let Outcome::Failed(_, full) = &outcome {
            for l in full.lines().chain(detail.lines()) {
                log.push_str(&format!("                 {l}\r\n"));
            }
        }
        match outcome {
            Outcome::Updated(_) => summary.updated.push(name),
            Outcome::UpToDate(_) => summary.up_to_date += 1,
            Outcome::Skipped(why) => summary.skipped.push((name, why)),
            Outcome::Failed(why, _) => summary.failed.push((name, why)),
        }
    }
    if let Some(p) = log_path(app) {
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(p, log);
    }
    summary
}

struct Out {
    ok: bool,
    stdout: String,
    stderr: String,
}

fn git(repo: &Path, args: &[&str]) -> Out {
    let child = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GCM_INTERACTIVE", "never")
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn();
    let mut child = match child {
        Ok(c) => c,
        Err(e) => return Out { ok: false, stdout: String::new(), stderr: format!("git non trovato: {e}") },
    };
    // read the pipes on threads so a chatty git can't block on a full buffer
    let mut so = child.stdout.take();
    let mut se = child.stderr.take();
    let t_out = std::thread::spawn(move || {
        let mut s = String::new();
        if let Some(p) = so.as_mut() {
            let _ = p.read_to_string(&mut s);
        }
        s
    });
    let t_err = std::thread::spawn(move || {
        let mut s = String::new();
        if let Some(p) = se.as_mut() {
            let _ = p.read_to_string(&mut s);
        }
        s
    });
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(st)) => break Some(st),
            Ok(None) if started.elapsed() > TIMEOUT => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(_) => break None,
        }
    };
    let stdout = t_out.join().unwrap_or_default();
    let mut stderr = t_err.join().unwrap_or_default();
    if status.is_none() {
        stderr.push_str("\nTempo scaduto (3 minuti)");
    }
    Out { ok: status.is_some_and(|s| s.success()), stdout, stderr }
}

fn last_line(s: &str) -> String {
    s.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with("hint:")).last().unwrap_or("errore sconosciuto").to_string()
}

fn has_ref(repo: &Path, r: &str) -> bool {
    git(repo, &["rev-parse", "--verify", "--quiet", r]).ok
}

/// main if it exists (locally or on origin), otherwise master.
fn main_branch(repo: &Path) -> Option<&'static str> {
    ["main", "master"]
        .into_iter()
        .find(|b| has_ref(repo, &format!("refs/heads/{b}")) || has_ref(repo, &format!("refs/remotes/origin/{b}")))
}

fn repos_in_root() -> Result<Vec<PathBuf>, String> {
    let root = PathBuf::from(crate::settings::get().git_root.trim());
    let mut repos: Vec<PathBuf> = std::fs::read_dir(&root)
        .map_err(|_| format!("Cartella non trovata: {}", root.display()))?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.join(".git").exists())
        .collect();
    repos.sort_by_key(|p| p.file_name().map(|n| n.to_ascii_lowercase()));
    Ok(repos)
}

/* ---------- status on hover (no pull) ---------- */

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct RepoStatus {
    name: String,
    /// checked-out branch ("" if detached)
    branch: String,
    main: String,
    /// commits on origin/<main> missing from the local <main>
    behind: u32,
    /// local commits on <main> not on origin
    ahead: u32,
    dirty: bool,
    error: Option<String>,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct GitStatus {
    repos: Vec<RepoStatus>,
    error: Option<String>,
    /// ms since epoch of the check
    checked_ms: i64,
}

static STATUS: Mutex<Option<(Instant, GitStatus)>> = Mutex::new(None);
const STATUS_TTL: Duration = Duration::from_secs(120);

/// Status of every repository (fetch only). `force` skips the 2-minute cache.
#[tauri::command]
pub async fn git_status_all(force: Option<bool>) -> Result<GitStatus, String> {
    tauri::async_runtime::spawn_blocking(move || {
        // holding the lock while checking: a second hover waits and reuses the result
        let mut cache = STATUS.lock().map_err(|e| e.to_string())?;
        if let Some((at, s)) = cache.as_ref() {
            if !force.unwrap_or(false) && at.elapsed() < STATUS_TTL {
                return Ok(s.clone());
            }
        }
        let s = status_all();
        *cache = Some((Instant::now(), s.clone()));
        Ok(s)
    })
    .await
    .map_err(|e| e.to_string())?
}

fn status_all() -> GitStatus {
    let checked_ms = chrono::Utc::now().timestamp_millis();
    let repos = match repos_in_root() {
        Ok(r) => r,
        Err(e) => return GitStatus { repos: vec![], error: Some(e), checked_ms },
    };
    let queue = Arc::new(Mutex::new(repos.into_iter().collect::<VecDeque<_>>()));
    let out = Arc::new(Mutex::new(Vec::new()));
    let workers: Vec<_> = (0..WORKERS)
        .map(|_| {
            let (queue, out) = (queue.clone(), out.clone());
            std::thread::spawn(move || loop {
                let Some(repo) = queue.lock().ok().and_then(|mut q| q.pop_front()) else { break };
                let s = status_one(&repo);
                if let Ok(mut o) = out.lock() {
                    o.push(s);
                }
            })
        })
        .collect();
    for w in workers {
        let _ = w.join();
    }
    let mut repos = std::mem::take(&mut *out.lock().unwrap());
    repos.sort_by_key(|r| r.name.to_lowercase());
    GitStatus { repos, error: None, checked_ms }
}

fn status_one(repo: &Path) -> RepoStatus {
    let name = repo.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let cur = git(repo, &["symbolic-ref", "--short", "-q", "HEAD"]);
    let branch = if cur.ok { cur.stdout.trim().to_string() } else { String::new() };
    let dirty = !git(repo, &["status", "--porcelain", "--untracked-files=no"]).stdout.trim().is_empty();
    let mut s = RepoStatus { name, branch, main: String::new(), behind: 0, ahead: 0, dirty, error: None };
    let Some(main) = main_branch(repo) else {
        s.error = Some("nessun branch main o master".into());
        return s;
    };
    s.main = main.to_string();
    let fetch = git(repo, &["fetch", "--quiet", "--no-tags", "origin", main]);
    if !fetch.ok {
        s.error = Some(format!("fetch non riuscito: {}", last_line(&fetch.stderr)));
    }
    let remote = format!("refs/remotes/origin/{main}");
    let local = format!("refs/heads/{main}");
    if !has_ref(repo, &remote) {
        s.error.get_or_insert_with(|| "nessun branch remoto".into());
        return s;
    }
    let count = |range: String| git(repo, &["rev-list", "--count", &range]).stdout.trim().parse::<u32>().unwrap_or(0);
    if has_ref(repo, &local) {
        s.behind = count(format!("{local}..{remote}"));
        s.ahead = count(format!("{remote}..{local}"));
    } else {
        // main only on origin: the pull would create it, nothing is behind
        s.behind = 0;
    }
    s
}

fn pull_one(repo: &Path, log: &mut String) -> Outcome {
    let git_dir = {
        let o = git(repo, &["rev-parse", "--git-dir"]);
        if !o.ok {
            return Outcome::Failed("non è un repository valido".into(), o.stderr);
        }
        let d = PathBuf::from(o.stdout.trim());
        if d.is_absolute() { d } else { repo.join(d) }
    };
    if git_dir.join("rebase-merge").exists() || git_dir.join("rebase-apply").exists() {
        return Outcome::Skipped("rebase già in corso".into());
    }
    if git_dir.join("MERGE_HEAD").exists() {
        return Outcome::Skipped("merge in corso".into());
    }
    let current = git(repo, &["symbolic-ref", "--short", "-q", "HEAD"]);
    let current = if current.ok { current.stdout.trim().to_string() } else { "HEAD staccato".to_string() };
    let status = git(repo, &["status", "--porcelain", "--untracked-files=no"]);
    if !status.stdout.trim().is_empty() {
        return Outcome::Skipped(format!("{current}: modifiche locali non committate"));
    }

    // Always work on the main branch: main if it exists (locally or on origin), otherwise master.
    let has = |r: String| has_ref(repo, &r);
    let Some(main) = main_branch(repo) else {
        return Outcome::Skipped(format!("{current}: nessun branch main o master"));
    };
    let mut note = String::new();
    if current != main {
        // creates the local branch tracking origin/<main> if it doesn't exist yet
        let sw = git(repo, &["switch", main]);
        if !sw.ok {
            return Outcome::Failed(format!("impossibile passare da {current} a {main}: {}", last_line(&sw.stderr)), sw.stderr);
        }
        note = format!("{current} → {main}");
    }
    if !git(repo, &["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"]).ok {
        if has(format!("refs/remotes/origin/{main}")) {
            git(repo, &["branch", &format!("--set-upstream-to=origin/{main}"), main]);
        } else {
            return Outcome::Skipped(format!("{main}: nessun branch remoto collegato"));
        }
    }
    let label = if note.is_empty() { main.to_string() } else { note.clone() };

    let before = git(repo, &["rev-parse", "HEAD"]).stdout.trim().to_string();
    let pull = git(repo, &["pull", "--rebase", "--no-autostash"]);
    log.push_str(&pull.stdout);
    if !pull.ok {
        let msg = last_line(&pull.stderr);
        if git_dir.join("rebase-merge").exists() || git_dir.join("rebase-apply").exists() {
            git(repo, &["rebase", "--abort"]);
            return Outcome::Failed(format!("{label}: conflitto, rebase annullato"), pull.stderr);
        }
        return Outcome::Failed(format!("{label}: {msg}"), pull.stderr);
    }
    let after = git(repo, &["rev-parse", "HEAD"]).stdout.trim().to_string();
    if before == after {
        return Outcome::UpToDate(note);
    }
    let n = git(repo, &["rev-list", "--count", &format!("{before}..{after}")]).stdout.trim().to_string();
    Outcome::Updated(format!("{label} +{n} commit"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sh(dir: &Path, args: &[&str]) {
        let o = git(dir, args);
        assert!(o.ok, "git {:?} failed: {}", args, o.stderr);
    }
    fn write(p: &Path, s: &str) {
        std::fs::write(p, s).unwrap();
    }

    #[test]
    fn pull_cases() {
        let root = std::env::temp_dir().join(format!("lumen-git-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let origin = root.join("origin.git");
        std::fs::create_dir_all(&origin).unwrap();
        sh(&origin, &["init", "--bare", "-b", "main"]);
        let a = root.join("a");
        let b = root.join("b");
        for d in [&a, &b] {
            sh(&root, &["clone", "-q", origin.to_str().unwrap(), d.to_str().unwrap()]);
            sh(d, &["config", "user.email", "t@t"]);
            sh(d, &["config", "user.name", "t"]);
            sh(d, &["config", "commit.gpgsign", "false"]);
        }
        write(&a.join("f.txt"), "1\n");
        sh(&a, &["add", "."]);
        sh(&a, &["commit", "-qm", "one"]);
        sh(&a, &["push", "-q", "origin", "HEAD:main"]);
        sh(&b, &["fetch", "-q"]);
        sh(&b, &["checkout", "-q", "-b", "main", "--track", "origin/main"]);

        // up to date
        let mut log = String::new();
        assert!(matches!(pull_one(&b, &mut log), Outcome::UpToDate(ref n) if n.is_empty()));

        // new commit upstream → updated
        write(&a.join("f.txt"), "2\n");
        sh(&a, &["commit", "-qam", "two"]);
        sh(&a, &["push", "-q"]);
        assert!(matches!(pull_one(&b, &mut log), Outcome::Updated(ref s) if s.contains("+1")));

        // local uncommitted change → skipped, file untouched
        write(&b.join("f.txt"), "local\n");
        assert!(matches!(pull_one(&b, &mut log), Outcome::Skipped(_)));
        assert_eq!(std::fs::read_to_string(b.join("f.txt")).unwrap(), "local\n");
        sh(&b, &["checkout", "--", "f.txt"]);

        // conflicting local commit → failed, rebase aborted, local commit kept
        write(&b.join("f.txt"), "mine\n");
        sh(&b, &["commit", "-qam", "mine"]);
        write(&a.join("f.txt"), "theirs\n");
        sh(&a, &["commit", "-qam", "theirs"]);
        sh(&a, &["push", "-q"]);
        assert!(matches!(pull_one(&b, &mut log), Outcome::Failed(ref s, _) if s.contains("conflitto")));
        assert!(!b.join(".git").join("rebase-merge").exists() && !b.join(".git").join("rebase-apply").exists());
        assert_eq!(std::fs::read_to_string(b.join("f.txt")).unwrap().trim(), "mine");

        // on a feature branch → switches to main, then pulls
        sh(&b, &["reset", "-q", "--hard", "origin/main"]);
        sh(&b, &["checkout", "-q", "-b", "feature"]);
        write(&a.join("g.txt"), "new\n");
        sh(&a, &["add", "."]);
        sh(&a, &["commit", "-qm", "three"]);
        sh(&a, &["push", "-q"]);

        // status on hover: fetch only, reports behind on main while on feature, pulls nothing
        let head_before = git(&b, &["rev-parse", "HEAD"]).stdout;
        let st = status_one(&b);
        assert_eq!((st.main.as_str(), st.branch.as_str(), st.behind, st.ahead, st.dirty), ("main", "feature", 1, 0, false));
        assert!(st.error.is_none());
        assert_eq!(git(&b, &["rev-parse", "HEAD"]).stdout, head_before);
        assert!(!b.join("g.txt").exists());
        let r = pull_one(&b, &mut log);
        assert!(matches!(r, Outcome::Updated(ref s) if s.starts_with("feature → main") && s.contains("+1")));
        assert_eq!(git(&b, &["branch", "--show-current"]).stdout.trim(), "main");

        // detached HEAD → back on main
        sh(&a, &["checkout", "-q", "--detach"]);
        assert!(matches!(pull_one(&a, &mut log), Outcome::UpToDate(ref n) if n.contains("→ main")));
        assert_eq!(git(&a, &["branch", "--show-current"]).stdout.trim(), "main");

        // master-only repository: a fresh clone of a repo whose branch is master
        let m_origin = root.join("m.git");
        std::fs::create_dir_all(&m_origin).unwrap();
        sh(&m_origin, &["init", "--bare", "-b", "master"]);
        let m = root.join("m");
        sh(&root, &["clone", "-q", m_origin.to_str().unwrap(), m.to_str().unwrap()]);
        sh(&m, &["config", "user.email", "t@t"]);
        sh(&m, &["config", "user.name", "t"]);
        sh(&m, &["checkout", "-q", "-b", "master"]);
        write(&m.join("x.txt"), "x\n");
        sh(&m, &["add", "."]);
        sh(&m, &["commit", "-qm", "x"]);
        sh(&m, &["push", "-q", "-u", "origin", "master"]);
        sh(&m, &["checkout", "-q", "-b", "work"]);
        assert!(matches!(pull_one(&m, &mut log), Outcome::UpToDate(ref n) if n == "work → master"));

        let _ = std::fs::remove_dir_all(&root);
    }
}
