//! Calendar events for the notch, from one of two sources (settings → calendarSource):
//!
//! * "outlook": classic Outlook through COM (outlook-calendar.ps1, run hidden). If Outlook
//!   isn't open it is started invisibly, synced for a few seconds and closed again.
//!   Every 10 minutes. The new Outlook has no API, but classic Outlook shares the account.
//! * "ics": a published ICS link (Outlook on the web → Settings → Calendar → Shared
//!   calendars → Publish a calendar), every 5 minutes. Recurring meetings are expanded
//!   for a window around today, times are converted to UTC with Windows' own time-zone
//!   database (Outlook writes Windows zone names such as "W. Europe Standard Time").
//!
//! In both cases the Teams / Zoom / Meet join link is extracted. Emits `agenda`.

use std::collections::{HashMap, HashSet};
use std::sync::{Condvar, Mutex};
use std::time::Duration;

use chrono::{Datelike, Duration as CDuration, Local, NaiveDate, NaiveDateTime, NaiveTime, Weekday};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager};

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Event {
    id: String,
    title: String,
    /// Unix milliseconds (UTC).
    start: i64,
    end: i64,
    all_day: bool,
    location: String,
    join: Option<String>,
    free: bool,
}

#[derive(Serialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct Agenda {
    configured: bool,
    events: Vec<Event>,
    error: Option<String>,
    updated_ms: i64,
}

static LATEST: Mutex<Option<Agenda>> = Mutex::new(None);
static WAKE: (Mutex<bool>, Condvar) = (Mutex::new(false), Condvar::new());

const REFRESH: Duration = Duration::from_secs(5 * 60);
const RETRY: Duration = Duration::from_secs(60);
const OUTLOOK_REFRESH: Duration = Duration::from_secs(10 * 60);
const OUTLOOK_RETRY: Duration = Duration::from_secs(3 * 60);
const OUTLOOK_SCRIPT: &str = include_str!("outlook-calendar.ps1");

#[tauri::command]
pub fn agenda_snapshot() -> Option<Agenda> {
    LATEST.lock().ok().and_then(|a| a.clone())
}

/// Download the calendar again now (settings changed, or "Aggiorna" pressed).
#[tauri::command]
pub fn refresh_agenda() {
    let (lock, cv) = &WAKE;
    if let Ok(mut w) = lock.lock() {
        *w = true;
        cv.notify_all();
    }
}

pub fn start(app: AppHandle) {
    std::thread::spawn(move || {
        unsafe {
            let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_MULTITHREADED);
        }
        let mut last_key = String::new();
        loop {
            let s = crate::settings::get();
            let url = s.calendar_url.trim().to_string();
            let key = format!("{}|{}", s.calendar_source, url);
            let previous = agenda_snapshot().filter(|_| key == last_key);
            let (result, mut wait, retry) = match s.calendar_source.as_str() {
                "outlook" => (Some(read_outlook(&app)), OUTLOOK_REFRESH, OUTLOOK_RETRY),
                "ics" if !url.is_empty() => (Some(fetch(&url).and_then(|text| parse_calendar(&text))), REFRESH, RETRY),
                _ => (None, REFRESH, RETRY),
            };
            let agenda = match result {
                None => Agenda::default(),
                Some(Ok(events)) => Agenda { configured: true, events, error: None, updated_ms: now_ms() },
                Some(Err(e)) => {
                    wait = retry;
                    // keep the last good events, with the error alongside
                    let mut a = previous.unwrap_or_default();
                    a.configured = true;
                    a.error = Some(e);
                    a
                }
            };
            last_key = key;
            if let Ok(mut l) = LATEST.lock() {
                *l = Some(agenda.clone());
            }
            let _ = app.emit("agenda", agenda);

            let (lock, cv) = &WAKE;
            if let Ok(guard) = lock.lock() {
                if let Ok((mut g, _)) = cv.wait_timeout_while(guard, wait, |w| !*w) {
                    *g = false;
                }
            }
        }
    });
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/* ------------------------------------------------------------------ */
/* Classic Outlook (COM, through a hidden PowerShell)                  */
/* ------------------------------------------------------------------ */

fn read_outlook(app: &AppHandle) -> Result<Vec<Event>, String> {
    // whatever classic Outlook notifies while we use it duplicates the new Outlook
    let ignore = |secs: i64| {
        crate::notify::IGNORE_CLASSIC_OUTLOOK_UNTIL.store(now_ms() + secs * 1000, std::sync::atomic::Ordering::Relaxed)
    };
    ignore(200);
    let result = run_outlook_script(app);
    ignore(20);
    result
}

fn run_outlook_script(app: &AppHandle) -> Result<Vec<Event>, String> {
    use std::io::Read;
    use std::os::windows::process::CommandExt;
    use std::process::{Command, Stdio};
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    let dir = app.path().app_config_dir().map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let script = dir.join("outlook-calendar.ps1");
    // UTF-8 with BOM, so Windows PowerShell 5.1 reads the non-ASCII text correctly
    let content = format!("\u{feff}{OUTLOOK_SCRIPT}");
    if std::fs::read_to_string(&script).ok().as_deref() != Some(content.as_str()) {
        std::fs::write(&script, &content).map_err(|e| e.to_string())?;
    }
    let mut child = Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-File"])
        .arg(&script)
        .args(["-SyncSeconds", "20"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map_err(|e| format!("PowerShell non disponibile ({e})"))?;
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
    let started = std::time::Instant::now();
    let ok = loop {
        match child.try_wait() {
            Ok(Some(st)) => break st.success(),
            Ok(None) if started.elapsed() > Duration::from_secs(150) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("Outlook classico non risponde".into());
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(200)),
            Err(e) => return Err(e.to_string()),
        }
    };
    let out = t_out.join().unwrap_or_default();
    let err = t_err.join().unwrap_or_default();
    if !ok {
        let line = err.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("errore sconosciuto");
        return Err(format!("Outlook classico non disponibile ({line})"));
    }
    let mut events: Vec<Event> =
        serde_json::from_str(out.trim()).map_err(|_| "Risposta di Outlook non valida".to_string())?;
    events.sort_by_key(|e| (e.start, e.end));
    Ok(events)
}

/* ------------------------------------------------------------------ */
/* Download                                                            */
/* ------------------------------------------------------------------ */

fn fetch(url: &str) -> Result<String, String> {
    use windows::core::{Interface, HSTRING};
    use windows::Foundation::Uri;
    use windows::Web::Http::Filters::{HttpBaseProtocolFilter, HttpCacheReadBehavior, IHttpFilter};
    use windows::Web::Http::HttpClient;

    let url = match url.strip_prefix("webcal://") {
        Some(rest) => format!("https://{rest}"),
        None => url.to_string(),
    };
    if !url.starts_with("https://") && !url.starts_with("http://") {
        return Err("Il link deve iniziare con https://".into());
    }
    let run = || -> windows::core::Result<String> {
        let filter = HttpBaseProtocolFilter::new()?;
        filter.CacheControl()?.SetReadBehavior(HttpCacheReadBehavior::MostRecent)?;
        let client = HttpClient::Create(&filter.cast::<IHttpFilter>()?)?;
        let uri = Uri::CreateUri(&HSTRING::from(url.as_str()))?;
        Ok(client.GetStringAsync(&uri)?.get()?.to_string())
    };
    let text = run().map_err(|e| format!("Calendario non raggiungibile ({})", e.message()))?;
    if !text.contains("BEGIN:VCALENDAR") {
        return Err("Il link non è un calendario ICS".into());
    }
    Ok(text)
}

/* ------------------------------------------------------------------ */
/* ICS parsing                                                         */
/* ------------------------------------------------------------------ */

struct Prop {
    name: String,
    params: HashMap<String, String>,
    value: String,
}

fn unfold(text: &str) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for raw in text.split('\n') {
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        if (line.starts_with(' ') || line.starts_with('\t')) && !lines.is_empty() {
            lines.last_mut().unwrap().push_str(&line[1..]);
        } else {
            lines.push(line.to_string());
        }
    }
    lines
}

fn parse_prop(line: &str) -> Option<Prop> {
    // the value starts at the first ':' that is not inside a quoted parameter
    let mut in_quotes = false;
    let mut split = None;
    for (i, c) in line.char_indices() {
        match c {
            '"' => in_quotes = !in_quotes,
            ':' if !in_quotes => {
                split = Some(i);
                break;
            }
            _ => {}
        }
    }
    let i = split?;
    let (head, value) = (&line[..i], &line[i + 1..]);
    let mut parts = head.split(';');
    let name = parts.next()?.to_ascii_uppercase();
    let params = parts
        .filter_map(|p| p.split_once('='))
        .map(|(k, v)| (k.to_ascii_uppercase(), v.trim_matches('"').to_string()))
        .collect();
    Some(Prop { name, params, value: value.to_string() })
}

fn unescape(v: &str) -> String {
    let mut out = String::with_capacity(v.len());
    let mut chars = v.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') | Some('N') => out.push('\n'),
                Some(o) => out.push(o),
                None => {}
            }
        } else {
            out.push(c);
        }
    }
    out
}

#[derive(Clone)]
enum Dt {
    Date(NaiveDate),
    Floating(NaiveDateTime, Option<String>),
    Utc(NaiveDateTime),
}

impl Dt {
    fn parse(p: &Prop) -> Option<Dt> {
        Self::parse_value(p.value.trim(), p.params.get("TZID").cloned(), p.params.get("VALUE").map(|v| v == "DATE"))
    }
    fn parse_value(v: &str, tz: Option<String>, is_date: Option<bool>) -> Option<Dt> {
        if is_date == Some(true) || v.len() == 8 {
            return NaiveDate::parse_from_str(&v[..8.min(v.len())], "%Y%m%d").ok().map(Dt::Date);
        }
        if let Some(u) = v.strip_suffix('Z') {
            return NaiveDateTime::parse_from_str(u, "%Y%m%dT%H%M%S").ok().map(Dt::Utc);
        }
        NaiveDateTime::parse_from_str(v, "%Y%m%dT%H%M%S").ok().map(|n| Dt::Floating(n, tz))
    }
    fn naive(&self) -> NaiveDateTime {
        match self {
            Dt::Date(d) => d.and_time(NaiveTime::MIN),
            Dt::Floating(n, _) | Dt::Utc(n) => *n,
        }
    }
    fn is_date(&self) -> bool {
        matches!(self, Dt::Date(_))
    }
    /// Same zone/kind as `self`, at another wall-clock time.
    fn with(&self, n: NaiveDateTime) -> Dt {
        match self {
            Dt::Date(_) => Dt::Date(n.date()),
            Dt::Floating(_, tz) => Dt::Floating(n, tz.clone()),
            Dt::Utc(_) => Dt::Utc(n),
        }
    }
    fn utc_ms(&self) -> i64 {
        match self {
            Dt::Utc(n) => n.and_utc().timestamp_millis(),
            Dt::Date(d) => local_to_utc_ms(d.and_time(NaiveTime::MIN), None),
            Dt::Floating(n, tz) => local_to_utc_ms(*n, tz.as_deref()),
        }
    }
}

#[derive(Default)]
struct VEvent {
    uid: String,
    summary: String,
    location: String,
    description: String,
    teams_url: String,
    start: Option<Dt>,
    end: Option<Dt>,
    duration_ms: Option<i64>,
    rrule: Option<String>,
    exdates: Vec<i64>,
    recurrence_id: Option<i64>,
    cancelled: bool,
    free: bool,
}

fn parse_calendar(text: &str) -> Result<Vec<Event>, String> {
    let mut events: Vec<VEvent> = Vec::new();
    let mut cur: Option<VEvent> = None;
    let mut depth_other = 0; // inside VALARM etc. within an event
    for line in unfold(text) {
        let Some(p) = parse_prop(&line) else { continue };
        match (p.name.as_str(), p.value.trim()) {
            ("BEGIN", "VEVENT") => {
                cur = Some(VEvent::default());
                depth_other = 0;
                continue;
            }
            ("END", "VEVENT") => {
                if let Some(e) = cur.take() {
                    events.push(e);
                }
                continue;
            }
            ("BEGIN", _) if cur.is_some() => {
                depth_other += 1;
                continue;
            }
            ("END", _) if cur.is_some() => {
                depth_other -= 1;
                continue;
            }
            _ => {}
        }
        let Some(e) = cur.as_mut() else { continue };
        if depth_other > 0 {
            continue;
        }
        match p.name.as_str() {
            "UID" => e.uid = p.value.clone(),
            "SUMMARY" => e.summary = unescape(&p.value),
            "LOCATION" => e.location = unescape(&p.value),
            "DESCRIPTION" => e.description = unescape(&p.value),
            "X-MICROSOFT-SKYPETEAMSMEETINGURL" => e.teams_url = p.value.trim().to_string(),
            "DTSTART" => e.start = Dt::parse(&p),
            "DTEND" => e.end = Dt::parse(&p),
            "DURATION" => e.duration_ms = parse_duration(&p.value),
            "RRULE" => e.rrule = Some(p.value.clone()),
            "EXDATE" => {
                let tz = p.params.get("TZID").cloned();
                let is_date = p.params.get("VALUE").map(|v| v == "DATE");
                for v in p.value.split(',') {
                    if let Some(d) = Dt::parse_value(v.trim(), tz.clone(), is_date) {
                        e.exdates.push(d.utc_ms());
                    }
                }
            }
            "RECURRENCE-ID" => e.recurrence_id = Dt::parse(&p).map(|d| d.utc_ms()),
            "STATUS" => e.cancelled = p.value.trim().eq_ignore_ascii_case("CANCELLED"),
            "X-MICROSOFT-CDO-BUSYSTATUS" => e.free = p.value.trim().eq_ignore_ascii_case("FREE"),
            "TRANSP" => e.free = e.free || p.value.trim().eq_ignore_ascii_case("TRANSPARENT"),
            _ => {}
        }
    }

    // window: a week before the start of this month → two months ahead
    let today = Local::now().date_naive();
    let month_start = today.with_day(1).unwrap_or(today);
    let win_start = local_to_utc_ms((month_start - CDuration::days(7)).and_time(NaiveTime::MIN), None);
    let win_end_naive = (today + CDuration::days(62)).and_time(NaiveTime::MIN);
    let win_end = local_to_utc_ms(win_end_naive, None);

    // occurrences replaced by an edited copy (same UID + RECURRENCE-ID)
    let overridden: HashSet<(String, i64)> = events
        .iter()
        .filter_map(|e| e.recurrence_id.map(|r| (e.uid.clone(), r)))
        .collect();

    let mut out = Vec::new();
    for e in &events {
        let Some(start) = e.start.clone() else { continue };
        if e.cancelled {
            continue;
        }
        let start_ms = start.utc_ms();
        let dur = match (&e.end, e.duration_ms) {
            (Some(end), _) => (end.utc_ms() - start_ms).max(0),
            (None, Some(d)) => d,
            (None, None) if start.is_date() => 86_400_000,
            _ => 0,
        };
        let make = |s: i64| Event {
            id: format!("{}#{}", e.uid, s),
            title: if e.summary.is_empty() { "Impegno".into() } else { e.summary.clone() },
            start: s,
            end: s + dur,
            all_day: start.is_date(),
            location: e.location.clone(),
            join: join_link(e),
            free: e.free,
        };
        match (&e.rrule, e.recurrence_id) {
            (Some(rule), None) => {
                let until = rule_part(rule, "UNTIL").and_then(|u| Dt::parse_value(u, None, None)).map(|d| match d {
                    // UNTIL without Z is in the event's own zone
                    Dt::Floating(n, _) => start.with(n).utc_ms(),
                    other => other.utc_ms(),
                });
                for n in expand(start.naive(), rule, win_end_naive + CDuration::days(2)) {
                    let s = start.with(n).utc_ms();
                    if until.is_some_and(|u| s > u) {
                        break;
                    }
                    if s > win_end {
                        break;
                    }
                    if s + dur < win_start || e.exdates.contains(&s) || overridden.contains(&(e.uid.clone(), s)) {
                        continue;
                    }
                    out.push(make(s));
                }
            }
            _ => {
                if start_ms <= win_end && start_ms + dur >= win_start {
                    out.push(make(start_ms));
                }
            }
        }
    }
    out.sort_by_key(|e| (e.start, e.end));
    out.truncate(800);
    Ok(out)
}

fn parse_duration(v: &str) -> Option<i64> {
    // P1D, PT30M, PT1H30M, P1W
    let v = v.trim().trim_start_matches(['+', 'P']);
    let (mut total, mut num, mut in_time) = (0i64, String::new(), false);
    for c in v.chars() {
        match c {
            'T' => in_time = true,
            '0'..='9' => num.push(c),
            _ => {
                let n: i64 = num.parse().ok()?;
                num.clear();
                total += n * match (c, in_time) {
                    ('W', _) => 604_800,
                    ('D', _) => 86_400,
                    ('H', true) => 3_600,
                    ('M', true) => 60,
                    ('S', true) => 1,
                    _ => return None,
                };
            }
        }
    }
    Some(total * 1000)
}

fn join_link(e: &VEvent) -> Option<String> {
    if e.teams_url.starts_with("https://") {
        return Some(e.teams_url.clone());
    }
    const HOSTS: [&str; 4] = ["teams.microsoft.com/", "teams.live.com/", "zoom.us/j/", "meet.google.com/"];
    for text in [&e.location, &e.description] {
        let mut rest = text.as_str();
        while let Some(i) = rest.find("https://") {
            let url: String = rest[i..]
                .chars()
                .take_while(|c| !c.is_whitespace() && !matches!(c, '<' | '>' | '"' | ')' | ']' | ','))
                .collect();
            if HOSTS.iter().any(|h| url.contains(h)) {
                return Some(url);
            }
            rest = &rest[i + 8..];
        }
    }
    None
}

/* ------------------------------------------------------------------ */
/* RRULE expansion (wall-clock times in the event's own zone)          */
/* ------------------------------------------------------------------ */

fn rule_part<'a>(rule: &'a str, key: &str) -> Option<&'a str> {
    rule.split(';').find_map(|p| p.split_once('=').filter(|(k, _)| k.eq_ignore_ascii_case(key)).map(|(_, v)| v))
}

fn weekday(code: &str) -> Option<Weekday> {
    Some(match code {
        "MO" => Weekday::Mon,
        "TU" => Weekday::Tue,
        "WE" => Weekday::Wed,
        "TH" => Weekday::Thu,
        "FR" => Weekday::Fri,
        "SA" => Weekday::Sat,
        "SU" => Weekday::Sun,
        _ => return None,
    })
}

/// "2TU" → (Some(2), Tue), "-1FR" → (Some(-1), Fri), "MO" → (None, Mon)
fn by_day(rule: &str) -> Vec<(Option<i32>, Weekday)> {
    rule_part(rule, "BYDAY")
        .map(|v| {
            v.split(',')
                .filter_map(|d| {
                    let d = d.trim();
                    let (num, code) = d.split_at(d.len().saturating_sub(2));
                    Some((num.parse::<i32>().ok(), weekday(code)?))
                })
                .collect()
        })
        .unwrap_or_default()
}

fn nums(rule: &str, key: &str) -> Vec<i32> {
    rule_part(rule, key).map(|v| v.split(',').filter_map(|n| n.trim().parse().ok()).collect()).unwrap_or_default()
}

fn days_in_month(y: i32, m: u32) -> u32 {
    let (ny, nm) = if m == 12 { (y + 1, 1) } else { (y, m + 1) };
    NaiveDate::from_ymd_opt(ny, nm, 1).map(|d| (d - CDuration::days(1)).day()).unwrap_or(28)
}

/// Days of month `y-m` matching the rule (BYDAY with ordinals, BYMONTHDAY, BYSETPOS).
fn month_days(y: i32, m: u32, rule: &str, default_day: u32) -> Vec<NaiveDate> {
    let last = days_in_month(y, m);
    let date = |d: u32| NaiveDate::from_ymd_opt(y, m, d);
    let mut days: Vec<NaiveDate> = Vec::new();
    let byday = by_day(rule);
    let bymonthday = nums(rule, "BYMONTHDAY");
    if !byday.is_empty() {
        for (ord, wd) in byday {
            let all: Vec<NaiveDate> = (1..=last).filter_map(date).filter(|d| d.weekday() == wd).collect();
            match ord {
                Some(n) if n > 0 => days.extend(all.get(n as usize - 1)),
                Some(n) if n < 0 => days.extend(all.len().checked_sub((-n) as usize).and_then(|i| all.get(i))),
                _ => days.extend(all),
            }
        }
    } else if !bymonthday.is_empty() {
        for n in bymonthday {
            let d = if n > 0 { n as u32 } else { (last as i32 + 1 + n).max(1) as u32 };
            days.extend(date(d).filter(|_| d <= last));
        }
    } else {
        days.extend(date(default_day).filter(|_| default_day <= last));
    }
    days.sort();
    days.dedup();
    let setpos = nums(rule, "BYSETPOS");
    if !setpos.is_empty() {
        let picked = setpos
            .iter()
            .filter_map(|&p| if p > 0 { days.get(p as usize - 1) } else { days.len().checked_sub((-p) as usize).and_then(|i| days.get(i)) })
            .copied()
            .collect::<Vec<_>>();
        days = picked;
        days.sort();
    }
    days
}

fn expand(start: NaiveDateTime, rule: &str, limit: NaiveDateTime) -> Vec<NaiveDateTime> {
    let freq = rule_part(rule, "FREQ").unwrap_or("").to_ascii_uppercase();
    let interval = rule_part(rule, "INTERVAL").and_then(|v| v.parse::<i64>().ok()).unwrap_or(1).max(1);
    let count = rule_part(rule, "COUNT").and_then(|v| v.parse::<usize>().ok());
    let time = start.time();
    let mut out = Vec::new();
    let push = |d: NaiveDate, out: &mut Vec<NaiveDateTime>| -> bool {
        let n = d.and_time(time);
        if n < start {
            return true;
        }
        if n > limit || count.is_some_and(|c| out.len() >= c) {
            return false;
        }
        out.push(n);
        true
    };
    const MAX_PERIODS: i64 = 4000;
    match freq.as_str() {
        "DAILY" => {
            let filter: Vec<Weekday> = by_day(rule).into_iter().map(|(_, w)| w).collect();
            for k in 0..MAX_PERIODS * 5 {
                let d = start.date() + CDuration::days(k * interval);
                if !filter.is_empty() && !filter.contains(&d.weekday()) {
                    if d.and_time(time) > limit {
                        break;
                    }
                    continue;
                }
                if !push(d, &mut out) {
                    break;
                }
            }
        }
        "WEEKLY" => {
            let mut days: Vec<Weekday> = by_day(rule).into_iter().map(|(_, w)| w).collect();
            if days.is_empty() {
                days.push(start.weekday());
            }
            days.sort_by_key(|w| w.num_days_from_monday());
            let week0 = start.date() - CDuration::days(start.weekday().num_days_from_monday() as i64);
            'weeks: for k in 0..MAX_PERIODS {
                let ws = week0 + CDuration::weeks(k * interval);
                for w in &days {
                    if !push(ws + CDuration::days(w.num_days_from_monday() as i64), &mut out) {
                        break 'weeks;
                    }
                }
            }
        }
        "MONTHLY" => {
            'months: for k in 0..MAX_PERIODS {
                let total = start.month0() as i64 + k * interval;
                let y = start.year() + (total / 12) as i32;
                let m = (total % 12) as u32 + 1;
                if NaiveDate::from_ymd_opt(y, m, 1).is_some_and(|d| d.and_time(time) > limit) {
                    break;
                }
                for d in month_days(y, m, rule, start.day()) {
                    if !push(d, &mut out) {
                        break 'months;
                    }
                }
            }
        }
        "YEARLY" => {
            let mut months: Vec<u32> = nums(rule, "BYMONTH").into_iter().map(|m| m as u32).collect();
            if months.is_empty() {
                months.push(start.month());
            }
            'years: for k in 0..MAX_PERIODS {
                let y = start.year() + (k * interval) as i32;
                if NaiveDate::from_ymd_opt(y, 1, 1).is_some_and(|d| d.and_time(time) > limit) {
                    break;
                }
                for &m in &months {
                    for d in month_days(y, m, rule, start.day()) {
                        if !push(d, &mut out) {
                            break 'years;
                        }
                    }
                }
            }
        }
        _ => out.push(start),
    }
    out
}

/* ------------------------------------------------------------------ */
/* Time zones: Windows' database, by Windows zone name                 */
/* ------------------------------------------------------------------ */

fn local_to_utc_ms(n: NaiveDateTime, tzid: Option<&str>) -> i64 {
    use windows::Win32::Foundation::SYSTEMTIME;
    use windows::Win32::System::Time::{
        EnumDynamicTimeZoneInformation, GetDynamicTimeZoneInformation, TzSpecificLocalTimeToSystemTimeEx,
        DYNAMIC_TIME_ZONE_INFORMATION,
    };

    thread_local! {
        static CACHE: std::cell::RefCell<HashMap<String, Option<DYNAMIC_TIME_ZONE_INFORMATION>>> = Default::default();
    }
    let lookup = |wanted: &str| -> Option<DYNAMIC_TIME_ZONE_INFORMATION> {
        let mut i = 0;
        loop {
            let mut z = DYNAMIC_TIME_ZONE_INFORMATION::default();
            if unsafe { EnumDynamicTimeZoneInformation(i, &mut z) } != 0 {
                return None;
            }
            let key = String::from_utf16_lossy(&z.TimeZoneKeyName);
            let key = key.trim_end_matches('\0');
            let std_name = String::from_utf16_lossy(&z.StandardName);
            if key.eq_ignore_ascii_case(wanted) || std_name.trim_end_matches('\0').eq_ignore_ascii_case(wanted) {
                return Some(z);
            }
            i += 1;
        }
    };
    let zone = tzid.map(str::trim).filter(|w| !w.is_empty()).and_then(|w| {
        CACHE.with(|c| *c.borrow_mut().entry(w.to_string()).or_insert_with(|| lookup(w)))
    });
    let tz = zone.unwrap_or_else(|| {
        // unknown or IANA name → the PC's own zone
        let mut z = DYNAMIC_TIME_ZONE_INFORMATION::default();
        unsafe { GetDynamicTimeZoneInformation(&mut z) };
        z
    });
    let local = SYSTEMTIME {
        wYear: n.year() as u16,
        wMonth: n.month() as u16,
        wDayOfWeek: 0,
        wDay: n.day() as u16,
        wHour: n.hour_u16(),
        wMinute: n.minute_u16(),
        wSecond: n.second_u16(),
        wMilliseconds: 0,
    };
    let mut utc = SYSTEMTIME::default();
    if unsafe { TzSpecificLocalTimeToSystemTimeEx(Some(&tz), &local, &mut utc) }.is_err() {
        return n.and_utc().timestamp_millis();
    }
    NaiveDate::from_ymd_opt(utc.wYear as i32, utc.wMonth as u32, utc.wDay as u32)
        .and_then(|d| d.and_hms_opt(utc.wHour as u32, utc.wMinute as u32, utc.wSecond as u32))
        .map(|u| u.and_utc().timestamp_millis())
        .unwrap_or_else(|| n.and_utc().timestamp_millis())
}

trait HmsU16 {
    fn hour_u16(&self) -> u16;
    fn minute_u16(&self) -> u16;
    fn second_u16(&self) -> u16;
}
impl HmsU16 for NaiveDateTime {
    fn hour_u16(&self) -> u16 {
        chrono::Timelike::hour(self) as u16
    }
    fn minute_u16(&self) -> u16 {
        chrono::Timelike::minute(self) as u16
    }
    fn second_u16(&self) -> u16 {
        chrono::Timelike::second(self) as u16
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_expands() {
        let today = Local::now().date_naive();
        let d = |off: i64| (today + CDuration::days(off)).format("%Y%m%d").to_string();
        let ics = format!(
            "BEGIN:VCALENDAR\r\nBEGIN:VTIMEZONE\r\nTZID:W. Europe Standard Time\r\nEND:VTIMEZONE\r\n\
BEGIN:VEVENT\r\nUID:a\r\nSUMMARY:Daily standup\r\nDTSTART;TZID=W. Europe Standard Time:{s}T093000\r\n\
DTEND;TZID=W. Europe Standard Time:{s}T094500\r\nRRULE:FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR\r\n\
EXDATE;TZID=W. Europe Standard Time:{x}T093000\r\nDESCRIPTION:Partecipa: https://teams.microsoft.com/l/meetup-join/19%3ameeting_x%40thread.v2/0?context=abc\\n\r\n\
BEGIN:VALARM\r\nTRIGGER:-PT15M\r\nEND:VALARM\r\nEND:VEVENT\r\n\
BEGIN:VEVENT\r\nUID:b\r\nSUMMARY:Review con\r\n  il team\r\nDTSTART:{t}T130000Z\r\nDTEND:{t}T140000Z\r\nEND:VEVENT\r\n\
BEGIN:VEVENT\r\nUID:c\r\nSUMMARY:Ferie\r\nDTSTART;VALUE=DATE:{t}\r\nDTEND;VALUE=DATE:{u}\r\nEND:VEVENT\r\n\
BEGIN:VEVENT\r\nUID:d\r\nSUMMARY:Annullata\r\nSTATUS:CANCELLED\r\nDTSTART:{t}T150000Z\r\nDTEND:{t}T160000Z\r\nEND:VEVENT\r\n\
BEGIN:VEVENT\r\nUID:e\r\nSUMMARY:Mensile\r\nDTSTART;TZID=W. Europe Standard Time:{s}T100000\r\nDURATION:PT30M\r\nRRULE:FREQ=MONTHLY;BYDAY=-1FR;COUNT=3\r\nEND:VEVENT\r\n\
END:VCALENDAR\r\n",
            s = d(-20),
            x = d(-1),
            t = d(1),
            u = d(2)
        );
        let ev = parse_calendar(&ics).unwrap();
        let standups: Vec<_> = ev.iter().filter(|e| e.title == "Daily standup").collect();
        assert!(standups.len() > 20, "standups: {}", standups.len());
        assert!(standups.iter().all(|e| e.end - e.start == 15 * 60 * 1000));
        assert!(standups[0].join.as_deref().unwrap().starts_with("https://teams.microsoft.com/l/meetup-join/"));
        let ex = local_to_utc_ms((today - CDuration::days(1)).and_hms_opt(9, 30, 0).unwrap(), Some("W. Europe Standard Time"));
        assert!(!standups.iter().any(|e| e.start == ex));
        assert!(ev.iter().any(|e| e.title == "Review con il team"));
        assert!(ev.iter().any(|e| e.title == "Ferie" && e.all_day));
        assert!(!ev.iter().any(|e| e.title == "Annullata"));
        let monthly: Vec<_> = ev.iter().filter(|e| e.title == "Mensile").collect();
        assert!(monthly.len() <= 3 && monthly.iter().all(|e| e.end - e.start == 30 * 60 * 1000));
        for e in &monthly {
            let dt = chrono::DateTime::from_timestamp_millis(e.start).unwrap().with_timezone(&Local);
            assert_eq!(dt.weekday(), Weekday::Fri);
        }
    }

    #[test]
    fn rome_offsets() {
        // winter: UTC+1, summer: UTC+2
        let w = NaiveDate::from_ymd_opt(2026, 1, 15).unwrap().and_hms_opt(10, 0, 0).unwrap();
        let s = NaiveDate::from_ymd_opt(2026, 7, 15).unwrap().and_hms_opt(10, 0, 0).unwrap();
        assert_eq!(local_to_utc_ms(w, Some("W. Europe Standard Time")), w.and_utc().timestamp_millis() - 3_600_000);
        assert_eq!(local_to_utc_ms(s, Some("W. Europe Standard Time")), s.and_utc().timestamp_millis() - 7_200_000);
        assert_eq!(local_to_utc_ms(w, Some("Eastern Standard Time")), w.and_utc().timestamp_millis() + 5 * 3_600_000);
    }
}

#[cfg(test)]
mod net_tests {
    #[test]
    #[ignore]
    fn fetch_public_ics() {
        unsafe {
            let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_MULTITHREADED);
        }
        let text = super::fetch("https://calendar.google.com/calendar/ical/it.italian%23holiday%40group.v.calendar.google.com/public/basic.ics").unwrap();
        let ev = super::parse_calendar(&text).unwrap();
        for e in ev.iter().take(5) {
            println!("{} {} all_day={}", chrono::DateTime::from_timestamp_millis(e.start).unwrap(), e.title, e.all_day);
        }
        println!("events in window: {}", ev.len());
        assert!(super::fetch("webcal://example.invalid/x.ics").is_err());
    }
}
