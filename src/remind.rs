//! Reminders: a quick capture that ends in `!when` comes back as a desktop
//! notification.
//!
//!     call the dentist !30m
//!     send the invoice !fri 10:00
//!     pick up liam !every mon 3pm, tue 1pm
//!
//! The inbox note is the only record: the line is written there with its time
//! (`⏰ 2026-09-21 09:00`, `⏰ every mon 15:00, tue 13:00`) and can be edited or
//! deleted like any other text. Nothing is ticked off when a reminder fires;
//! a line that repeats could not be anyway. Instead `omanote --remind`, run
//! once a minute by a systemd user timer, fires whatever came due since it
//! last looked, and remembers only when that was. So a reminder that came due
//! while the machine was asleep or off still arrives, late, and none arrives
//! twice.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const MARK: &str = "⏰";
const DAYS: [&str; 7] = ["mon", "tue", "wed", "thu", "fri", "sat", "sun"];
const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
const EVERY_DAY: u8 = 0b111_1111;
const WEEKDAYS: u8 = 0b001_1111;
const WEEKEND: u8 = 0b110_0000;
/// With no time given: the start of the working day.
const MORNING: u32 = 9 * 60;
/// How far back a first look after a long absence goes.
const CATCH_UP: i64 = 7 * 24 * 3600;
const LATE: i64 = 5 * 60;

/// A moment on the wall clock here. `weekday`: 0 is Monday.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Local {
    pub year: i32,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub weekday: u32,
}

pub fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

pub fn local(at: i64) -> Local {
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    let at = at as libc::time_t;
    unsafe { libc::localtime_r(&at, &mut tm) };
    Local {
        year: tm.tm_year + 1900,
        month: tm.tm_mon as u32 + 1,
        day: tm.tm_mday as u32,
        hour: tm.tm_hour as u32,
        minute: tm.tm_min as u32,
        weekday: (tm.tm_wday as u32 + 6) % 7,
    }
}

/// The wall-clock moment as a timestamp. Days and minutes may overflow
/// (the 32nd, minute 1500): the C library carries them over, summer time included.
pub fn stamp(year: i32, month: u32, day: i64, minute_of_day: i64) -> i64 {
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    tm.tm_year = year - 1900;
    tm.tm_mon = month as i32 - 1;
    tm.tm_mday = day as i32;
    tm.tm_min = minute_of_day as i32;
    tm.tm_isdst = -1;
    unsafe { libc::mktime(&mut tm) as i64 }
}

/// `days` days after the day `at` falls on, at `minute_of_day`.
fn day_at(at: i64, days: i64, minute_of_day: u32) -> i64 {
    let l = local(at);
    stamp(l.year, l.month, l.day as i64 + days, minute_of_day as i64)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum When {
    /// Once, at each of these moments.
    At(Vec<i64>),
    /// Every week: (days as a bit per weekday from Monday, minute of the day).
    Every(Vec<(u8, u32)>),
}

enum Day {
    Week(u8),
    /// Today, tomorrow.
    In(i64),
    Date(i32, u32, u32),
}

fn weekday(word: &str) -> Option<u32> {
    let word = word.trim_end_matches('s');
    let full = ["monday", "tuesday", "wednesday", "thursday", "friday", "saturday", "sunday"];
    let other = [("tues", 1), ("thur", 3), ("thurs", 3)];
    (0..7).find(|&i| word == DAYS[i] || word == full[i]).map(|i| i as u32).or_else(|| other.iter().find(|(w, _)| *w == word).map(|(_, d)| *d))
}

fn day(word: &str) -> Option<Day> {
    match word {
        "today" => return Some(Day::In(0)),
        "tomorrow" | "tmrw" => return Some(Day::In(1)),
        "day" | "daily" | "everyday" => return Some(Day::Week(EVERY_DAY)),
        "weekday" | "weekdays" | "workday" | "workdays" => return Some(Day::Week(WEEKDAYS)),
        "weekend" | "weekends" => return Some(Day::Week(WEEKEND)),
        _ => {}
    }
    if let Some(d) = weekday(word) {
        return Some(Day::Week(1 << d));
    }
    // mon-fri
    if let Some((a, b)) = word.split_once('-').and_then(|(a, b)| Some((weekday(a)?, weekday(b)?))) {
        let mut mask = 0u8;
        let mut d = a;
        loop {
            mask |= 1 << d;
            if d == b {
                break;
            }
            d = (d + 1) % 7;
        }
        return Some(Day::Week(mask));
    }
    // 2026-09-25
    let mut parts = word.split('-').map(|p| p.parse::<u32>().ok());
    match (parts.next()??, parts.next()??, parts.next()??, parts.next()) {
        (y, m @ 1..=12, d @ 1..=31, None) if y > 1999 => Some(Day::Date(y as i32, m, d)),
        _ => None,
    }
}

/// `15:30`, `9:05`, `3pm`, `3:30pm`, `12am`: minutes into the day.
fn time(word: &str) -> Option<u32> {
    let (clock, half) = match word.strip_suffix("am").map(|c| (c, Some(false))).or_else(|| word.strip_suffix("pm").map(|c| (c, Some(true)))) {
        Some(found) => found,
        None => (word, None),
    };
    let (h, m) = match clock.split_once(':') {
        Some((h, m)) if m.len() == 2 => (h.parse::<u32>().ok()?, m.parse::<u32>().ok()?),
        Some(_) => return None,
        // A bare number is only a time with am or pm behind it.
        None if half.is_some() => (clock.parse::<u32>().ok()?, 0),
        None => return None,
    };
    let h = match half {
        Some(_) if h == 0 || h > 12 => return None,
        Some(pm) => h % 12 + if pm { 12 } else { 0 },
        None => h,
    };
    (h < 24 && m < 60).then_some(h * 60 + m)
}

/// `30m`, `2h`, `1h30m`, `3d`, `1w`: seconds from now.
fn span(word: &str) -> Option<i64> {
    let mut total = 0i64;
    let mut digits = String::new();
    let mut unit = String::new();
    let mut flush = |digits: &mut String, unit: &mut String| -> Option<()> {
        let n: i64 = digits.parse().ok()?;
        let secs = match unit.as_str() {
            "m" | "min" | "mins" => 60,
            "h" | "hr" | "hrs" | "hour" | "hours" => 3600,
            "d" | "day" | "days" => 86400,
            "w" | "week" | "weeks" => 7 * 86400,
            _ => return None,
        };
        total += n.checked_mul(secs)?;
        digits.clear();
        unit.clear();
        Some(())
    };
    for c in word.chars() {
        if c.is_ascii_digit() {
            if !unit.is_empty() {
                flush(&mut digits, &mut unit)?;
            }
            digits.push(c);
        } else {
            if digits.is_empty() {
                return None;
            }
            unit.push(c);
        }
    }
    flush(&mut digits, &mut unit)?;
    (total > 0 && total < 5 * 366 * 86400).then_some(total)
}

/// What follows the `!`. Forgiving about how people write times: commas,
/// "at", "on" and "and" are passed over, and `1 pm` is `1pm`.
pub fn parse(spec: &str, now: i64) -> Option<When> {
    let lower = spec.to_lowercase().replace(',', " ");
    let mut words: Vec<String> = Vec::new();
    for word in lower.split_whitespace().filter(|w| !["at", "on", "and", "the", "@"].contains(w)) {
        match words.last_mut() {
            Some(last) if (word == "am" || word == "pm") && last.chars().next().is_some_and(|c| c.is_ascii_digit()) => last.push_str(word),
            _ => words.push(word.to_string()),
        }
    }
    let every = matches!(words.first().map(String::as_str), Some("every" | "each"));
    let words = &words[every as usize..];
    if words.is_empty() {
        return None;
    }
    if let ([only], false) = (words, every) {
        if let Some(secs) = span(only) {
            return Some(When::At(vec![now + secs]));
        }
    }

    // Days pile up until a time arrives: `mon wed 15:00, tue 13:00`.
    let mut slots: Vec<(Vec<Day>, u32)> = Vec::new();
    let mut days: Vec<Day> = Vec::new();
    for word in words {
        if let Some(t) = time(word) {
            slots.push((std::mem::take(&mut days), t));
        } else {
            days.push(day(word)?);
        }
    }
    if !days.is_empty() {
        slots.push((days, MORNING));
    }

    if every {
        let mut out = Vec::new();
        for (days, t) in slots {
            let mut mask = if days.is_empty() { EVERY_DAY } else { 0 };
            for d in days {
                match d {
                    Day::Week(m) => mask |= m,
                    _ => return None,
                }
            }
            out.push((mask, t));
        }
        return Some(When::Every(out));
    }

    let mut out = Vec::new();
    for (days, t) in slots {
        if days.is_empty() {
            // Just a time: today, or tomorrow if that has passed.
            let today = day_at(now, 0, t);
            out.push(if today > now { today } else { day_at(now, 1, t) });
        }
        for d in days {
            match d {
                Day::In(n) => out.push(day_at(now, n, t)),
                Day::Date(y, m, d) => out.push(stamp(y, m, d as i64, t as i64)),
                // The next such day, which is today if the time is still ahead.
                Day::Week(mask) => out.push((0..8).map(|n| day_at(now, n, t)).find(|&at| at > now && mask & (1 << local(at).weekday) != 0)?),
            }
        }
    }
    out.retain(|&at| at > now);
    out.sort_unstable();
    (!out.is_empty()).then_some(When::At(out))
}

fn clock(minute_of_day: u32) -> String {
    format!("{:02}:{:02}", minute_of_day / 60, minute_of_day % 60)
}

fn days_named(mask: u8) -> String {
    match mask {
        EVERY_DAY => "day".into(),
        WEEKDAYS => "weekday".into(),
        WEEKEND => "weekend".into(),
        _ => (0..7).filter(|d| mask & (1 << d) != 0).map(|d| DAYS[d]).collect::<Vec<_>>().join(" "),
    }
}

/// How a reminder is written into the note, and read back by `parse`.
pub fn render(when: &When) -> String {
    match when {
        When::At(times) => times
            .iter()
            .map(|&t| {
                let l = local(t);
                format!("{}-{:02}-{:02} {:02}:{:02}", l.year, l.month, l.day, l.hour, l.minute)
            })
            .collect::<Vec<_>>()
            .join(", "),
        When::Every(slots) => format!("every {}", slots.iter().map(|(mask, t)| format!("{} {}", days_named(*mask), clock(*t))).collect::<Vec<_>>().join(", ")),
    }
}

/// `Mon 21 Sep 15:00`, for people.
pub fn friendly(at: i64) -> String {
    let l = local(at);
    let day = DAYS[l.weekday as usize];
    format!("{}{} {} {} {:02}:{:02}", day[..1].to_uppercase(), &day[1..], l.day, MONTHS[l.month as usize - 1], l.hour, l.minute)
}

/// A capture that ends in `!when`: the text without it, and the reminder.
/// `Err` is what followed a `!` that could not be read as a time, so the
/// capture can say so; the text is then kept whole.
pub fn split(text: &str, now: i64) -> (String, Result<Option<When>, String>) {
    let at = if text.starts_with('!') { Some(0) } else { text.rfind(" !").map(|i| i + 1) };
    let Some(at) = at.filter(|&i| text[i + 1..].chars().next().is_some_and(|c| c.is_alphanumeric())) else {
        return (text.to_string(), Ok(None));
    };
    match parse(&text[at + 1..], now) {
        Some(when) => (text[..at].trim().to_string(), Ok(Some(when))),
        None => (text.to_string(), Err(text[at..].to_string())),
    }
}

/// A line of the note that carries a reminder: what to say, and when. A task
/// that has been ticked off (`- [x]`) no longer reminds.
pub fn in_line(line: &str, now: i64) -> Option<(String, When)> {
    let (text, spec) = line.split_once(MARK)?;
    let text = text.trim().trim_start_matches(['-', '*', '+']).trim_start();
    if text.starts_with("[x]") || text.starts_with("[X]") {
        return None;
    }
    let text = text.strip_prefix("[ ]").unwrap_or(text).trim_start();
    // The `14:02` a capture starts with says when it was written, not what.
    let text = match text.split_once(' ') {
        Some((first, rest)) if first.len() == 5 && time(first).is_some() => rest,
        _ => text,
    };
    let spec = spec.trim();
    // In the note a one-off is always a date, past or future. `tomorrow` would
    // mean something else every day it is read, so it is not accepted here.
    let dates: Option<Vec<i64>> = spec.split(',').map(|one| absolute(one.trim())).collect();
    let when = match dates {
        Some(times) if !times.is_empty() => When::At(times),
        _ => match parse(spec, now)? {
            every @ When::Every(_) => every,
            When::At(_) => return None,
        },
    };
    Some((text.trim().to_string(), when))
}

fn absolute(spec: &str) -> Option<i64> {
    let (date, clock) = spec.split_once(' ')?;
    match (day(date)?, time(clock)?) {
        (Day::Date(y, m, d), t) => Some(stamp(y, m, d as i64, t as i64)),
        _ => None,
    }
}

/// The latest moment in `(last, now]` at which this was due, if any.
pub fn due(when: &When, last: i64, now: i64) -> Option<i64> {
    match when {
        When::At(times) => times.iter().copied().filter(|&t| t > last && t <= now).max(),
        When::Every(slots) => (0..8)
            .flat_map(|back| slots.iter().map(move |&(mask, t)| (mask, day_at(now, -back, t))))
            .filter(|&(mask, at)| at > last && at <= now && mask & (1 << local(at).weekday) != 0)
            .map(|(_, at)| at)
            .max(),
    }
}

/// When it fires next, for the list.
pub fn next(when: &When, now: i64) -> Option<i64> {
    match when {
        When::At(times) => times.iter().copied().filter(|&t| t > now).min(),
        When::Every(slots) => (0..8)
            .flat_map(|ahead| slots.iter().map(move |&(mask, t)| (mask, day_at(now, ahead, t))))
            .filter(|&(mask, at)| at > now && mask & (1 << local(at).weekday) != 0)
            .map(|(_, at)| at)
            .min(),
    }
}

fn state_file(home: &Path) -> PathBuf {
    home.join("remind-state")
}

/// Everything in `note` that came due since the last look: (what, when it was due).
pub fn came_due(note: &str, last: i64, now: i64) -> Vec<(String, i64)> {
    note.lines().filter_map(|l| in_line(l, now)).filter_map(|(text, when)| due(&when, last, now).map(|at| (text, at))).collect()
}

/// `omanote --remind`: what the timer runs. Quiet unless something is due.
pub fn run(home: &Path, inbox: &Path) -> Result<usize, String> {
    let now = now();
    let state = state_file(home);
    // The first look ever starts now: old lines in the note are history.
    let last = std::fs::read_to_string(&state).ok().and_then(|s| s.trim().parse::<i64>().ok()).unwrap_or(now - 60).max(now - CATCH_UP);
    let note = std::fs::read_to_string(inbox).unwrap_or_default();
    let fired = came_due(&note, last, now);
    // Remember first: better a reminder lost to a crash than one repeated every minute.
    std::fs::create_dir_all(home).and_then(|_| std::fs::write(&state, now.to_string())).map_err(|e| format!("cannot write {}: {e}", state.display()))?;
    for (text, at) in &fired {
        let (title, body) = if now - at > LATE { (format!("Missed reminder · {}", friendly(*at)), text.as_str()) } else { ("Reminder".to_string(), text.as_str()) };
        notify(&title, if body.is_empty() { "It is time" } else { body });
    }
    Ok(fired.len())
}

fn on_path(program: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|paths| std::env::split_paths(&paths).any(|dir| dir.join(program).is_file()))
}

/// Omarchy's notifications where there are any, the desktop's otherwise.
pub fn notify(title: &str, body: &str) {
    let mut command = if on_path("omarchy-notification-send") {
        let mut c = Command::new("omarchy-notification-send");
        c.args(["-g", "󰢌", title, body]);
        c
    } else {
        let mut c = Command::new("notify-send");
        c.args(["-a", "omanote", "-u", "critical", title, body]);
        c
    };
    let _ = command.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status();
}

/// `omanote --reminders`: what is still to come, soonest first.
pub fn list(inbox: &Path) -> String {
    let now = now();
    let note = std::fs::read_to_string(inbox).unwrap_or_default();
    let mut coming: Vec<(i64, String)> = note
        .lines()
        .filter_map(|l| in_line(l, now))
        .filter_map(|(text, when)| {
            let at = next(&when, now)?;
            let how = if matches!(when, When::Every(_)) { format!("  ({})", render(&when)) } else { String::new() };
            Some((at, format!("{:<17} {text}{how}", friendly(at))))
        })
        .collect();
    coming.sort();
    let mut out = match coming.len() {
        0 => "No reminders. Add one with: omanote --capture \"call the dentist !tomorrow 9:00\"\n".to_string(),
        _ => coming.into_iter().map(|(_, line)| line + "\n").collect(),
    };
    out.push_str(&format!("\nThey live in {}\n{}\n", crate::vaults::tilde(inbox), timer_state()));
    out
}

// ---- the timer that does the looking -------------------------------------

const SERVICE: &str = "omanote-remind.service";
const TIMER: &str = "omanote-remind.timer";

fn unit_dir() -> Option<PathBuf> {
    let config = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(config.join("systemd/user"))
}

fn systemctl(args: &[&str]) -> bool {
    Command::new("systemctl").arg("--user").args(args).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok_and(|s| s.success())
}

pub fn timer_on() -> bool {
    systemctl(&["is-active", "--quiet", TIMER])
}

fn timer_state() -> String {
    if timer_on() { "The reminder timer is on (omanote --reminders off stops it).".into() } else { "The reminder timer is OFF: nothing will fire. Turn it on with: omanote --reminders on".into() }
}

/// A systemd user timer that runs `omanote --remind` on the minute. It carries
/// OMANOTE_HOME along if one is set, so the timer looks where you do.
pub fn turn_on() -> Result<String, String> {
    let dir = unit_dir().ok_or("cannot find your config folder")?;
    let exe = std::env::current_exe().map_err(|e| format!("cannot find omanote itself: {e}"))?;
    let home = std::env::var("OMANOTE_HOME").ok().filter(|h| !h.is_empty()).map(|h| format!("Environment=\"OMANOTE_HOME={h}\"\n")).unwrap_or_default();
    let service = format!("[Unit]\nDescription=omanote: fire the reminders that are due\n\n[Service]\nType=oneshot\n{home}ExecStart=\"{}\" --remind\n", exe.display());
    let timer = "[Unit]\nDescription=omanote: look for due reminders every minute\n\n[Timer]\nOnCalendar=*:*:00\nAccuracySec=1s\nPersistent=true\n\n[Install]\nWantedBy=timers.target\n";
    let write = |name: &str, text: &str| std::fs::write(dir.join(name), text).map_err(|e| format!("cannot write {}: {e}", dir.join(name).display()));
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    write(SERVICE, &service)?;
    write(TIMER, timer)?;
    systemctl(&["daemon-reload"]);
    if !systemctl(&["enable", "--now", TIMER]) {
        return Err(format!("wrote {}, but systemctl --user could not start it", dir.join(TIMER).display()));
    }
    Ok(format!("Reminders are on: {} runs {} --remind every minute", TIMER, exe.display()))
}

pub fn turn_off() -> Result<String, String> {
    let dir = unit_dir().ok_or("cannot find your config folder")?;
    systemctl(&["disable", "--now", TIMER]);
    for name in [SERVICE, TIMER] {
        let _ = std::fs::remove_file(dir.join(name));
    }
    systemctl(&["daemon-reload"]);
    Ok("Reminders are off. The lines stay in your inbox; omanote --reminders on brings them back.".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sunday 20 September 2026, 14:00, wherever the tests run.
    fn sunday() -> i64 {
        let at = stamp(2026, 9, 20, 14 * 60);
        assert_eq!(local(at), Local { year: 2026, month: 9, day: 20, hour: 14, minute: 0, weekday: 6 });
        at
    }

    fn at(spec: &str) -> Vec<String> {
        match parse(spec, sunday()) {
            Some(When::At(times)) => times.into_iter().map(|t| render(&When::At(vec![t]))).collect(),
            other => panic!("{spec}: {other:?}"),
        }
    }

    #[test]
    fn reads_times_the_way_people_write_them() {
        assert_eq!(at("30m"), ["2026-09-20 14:30"]);
        assert_eq!(at("1h30m"), ["2026-09-20 15:30"]);
        assert_eq!(at("2d"), ["2026-09-22 14:00"]);
        assert_eq!(at("15:30"), ["2026-09-20 15:30"], "later today");
        assert_eq!(at("9:00"), ["2026-09-21 09:00"], "already past today: tomorrow");
        assert_eq!(at("3pm"), ["2026-09-20 15:00"]);
        assert_eq!(at("tomorrow"), ["2026-09-21 09:00"], "no time: the morning");
        assert_eq!(at("Tomorrow at 7:15am"), ["2026-09-21 07:15"]);
        assert_eq!(at("fri 10:00"), ["2026-09-25 10:00"]);
        assert_eq!(at("sunday 18:00"), ["2026-09-20 18:00"], "today is Sunday and 18:00 is still ahead");
        assert_eq!(at("sun 9:00"), ["2026-09-27 09:00"], "9:00 has gone: next Sunday");
        assert_eq!(at("2026-12-24 8pm"), ["2026-12-24 20:00"]);
        assert_eq!(at("on Mon at 3pm, Tuesday 1 pm"), ["2026-09-21 15:00", "2026-09-22 13:00"]);
        assert_eq!(at("12am"), ["2026-09-21 00:00"]);
        for nonsense in ["", "soon", "important", "25:00", "13pm", "9", "fri 99:00", "2026-13-01 9:00", "2020-01-01 9:00", "0m"] {
            assert_eq!(parse(nonsense, sunday()), None, "{nonsense}");
        }
    }

    #[test]
    fn reads_what_repeats() {
        let every = |spec: &str| parse(spec, sunday()).map(|w| render(&w));
        assert_eq!(every("every mon 3pm, tue 1pm").as_deref(), Some("every mon 15:00, tue 13:00"));
        assert_eq!(every("every Monday at 3pm and Tuesday 1 pm").as_deref(), Some("every mon 15:00, tue 13:00"));
        assert_eq!(every("every mon, wed, fri 8:00").as_deref(), Some("every mon wed fri 08:00"));
        assert_eq!(every("every weekday 9:30").as_deref(), Some("every weekday 09:30"));
        assert_eq!(every("every mon-fri 9:30").as_deref(), Some("every weekday 09:30"));
        assert_eq!(every("every day 8am").as_deref(), Some("every day 08:00"));
        assert_eq!(every("every 8am").as_deref(), Some("every day 08:00"));
        assert_eq!(every("every sat-sun 10:00").as_deref(), Some("every weekend 10:00"));
        assert_eq!(every("every fri").as_deref(), Some("every fri 09:00"));
        assert_eq!(every("every tomorrow 9:00"), None);
        assert_eq!(every("every"), None);
        // What is written into the note reads back as the same thing.
        for spec in ["every mon 15:00, tue 13:00", "every weekday 09:30", "2026-09-25 10:00", "2026-09-21 15:00, 2026-09-22 13:00"] {
            assert_eq!(every(spec).as_deref(), Some(spec));
        }
    }

    #[test]
    fn takes_the_reminder_off_the_end_of_a_capture() {
        let (text, when) = split("call the dentist !tomorrow 9:00", sunday());
        assert_eq!((text.as_str(), when.unwrap().map(|w| render(&w)).as_deref()), ("call the dentist", Some("2026-09-21 09:00")));
        let (text, when) = split("!every weekday 9:30 standup", sunday());
        assert!(when.is_err(), "the time goes last, so words after it are not a time");
        assert_eq!(text, "!every weekday 9:30 standup");
        assert_eq!(split("that was great!", sunday()), ("that was great!".into(), Ok(None)));
        assert_eq!(split("wow ! really", sunday()), ("wow ! really".into(), Ok(None)));
        assert_eq!(split("so !important", sunday()), ("so !important".into(), Err("!important".into())), "kept whole, and said");
        let (text, when) = split("!30m", sunday());
        assert_eq!((text.as_str(), when.unwrap().is_some()), ("", true));
    }

    #[test]
    fn finds_reminders_in_the_note() {
        let now = sunday();
        let say = |line: &str| in_line(line, now).map(|(text, when)| (text, render(&when)));
        assert_eq!(say("- 14:02 call the dentist ⏰ 2026-09-21 09:00"), Some(("call the dentist".into(), "2026-09-21 09:00".into())));
        assert_eq!(say("- 09:10 old news ⏰ 2026-01-05 09:00"), Some(("old news".into(), "2026-01-05 09:00".into())), "a past date is still a date");
        assert_eq!(say("- [ ] pick up liam ⏰ every mon 15:00, tue 13:00"), Some(("pick up liam".into(), "every mon 15:00, tue 13:00".into())));
        assert_eq!(say("- [x] pick up liam ⏰ every mon 15:00"), None, "ticked off: it stops");
        assert_eq!(say("- 14:02 no reminder here"), None);
        assert_eq!(say("- 14:02 broken ⏰ whenever"), None);
    }

    #[test]
    fn fires_what_came_due_since_the_last_look_and_only_once() {
        let note = "# Inbox\n\n## 2026-09-20\n- 13:00 call the dentist ⏰ 2026-09-21 09:00\n- 13:05 pick up liam ⏰ every mon 15:00, tue 13:00\n- 13:06 long ago ⏰ 2026-09-01 08:00\n";
        let monday = |h: i64, m: i64| stamp(2026, 9, 21, h * 60 + m);
        let names = |last, now| came_due(note, last, now).into_iter().map(|(text, _)| text).collect::<Vec<_>>();

        assert!(names(monday(8, 58), monday(8, 59)).is_empty());
        assert_eq!(names(monday(8, 59), monday(9, 0)), ["call the dentist"], "on the minute");
        assert!(names(monday(9, 0), monday(9, 1)).is_empty(), "and not again a minute later");
        assert_eq!(names(monday(14, 59), monday(15, 0)), ["pick up liam"]);
        assert_eq!(names(stamp(2026, 9, 28, 14 * 60 + 59), stamp(2026, 9, 28, 15 * 60)), ["pick up liam"], "and the Monday after");
        assert_eq!(names(stamp(2026, 9, 22, 12 * 60 + 59), stamp(2026, 9, 22, 13 * 60)), ["pick up liam"], "Tuesday has its own time");
        assert!(names(stamp(2026, 9, 22, 14 * 60 + 59), stamp(2026, 9, 22, 15 * 60)).is_empty(), "Monday's time means nothing on a Tuesday");

        // The laptop slept from Sunday evening to Monday 16:00: both arrive, once each, late.
        let late = came_due(note, stamp(2026, 9, 20, 20 * 60), monday(16, 0));
        assert_eq!(late, [("call the dentist".to_string(), monday(9, 0)), ("pick up liam".to_string(), monday(15, 0))]);
        // Away for a fortnight: one "pick up liam", the latest, not one per week.
        assert_eq!(names(stamp(2026, 9, 21, 16 * 60), stamp(2026, 10, 7, 12 * 60)), ["pick up liam"]);

        assert_eq!(next(&When::Every(vec![(1, 15 * 60), (2, 13 * 60)]), sunday()), Some(monday(15, 0)));
        assert_eq!(friendly(monday(15, 0)), "Mon 21 Sep 15:00");
    }

    #[test]
    fn the_timer_run_remembers_when_it_last_looked() {
        let home = std::env::temp_dir().join(format!("omanote-remind-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        let inbox = home.join("inbox.md");
        // Due long ago: the very first look does not dig up history.
        std::fs::write(&inbox, "- 09:00 ancient ⏰ 2026-01-05 09:00\n").unwrap();
        assert_eq!(run(&home, &inbox), Ok(0));
        let looked: i64 = std::fs::read_to_string(state_file(&home)).unwrap().parse().unwrap();
        assert!((now() - looked).abs() < 5);
        assert_eq!(run(&home, &home.join("no-inbox-yet.md")), Ok(0), "no inbox is no problem");
        let _ = std::fs::remove_dir_all(home);
    }
}
