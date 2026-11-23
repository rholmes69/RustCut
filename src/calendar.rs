//! Content scheduling on the user's Google Calendar: release days, premieres and posting slots
//! ("Post the Falling Deeper teaser on TikTok") as calendar events with reminders, optionally
//! repeated daily or weekly (as separate events, each at the same local time even across a
//! daylight-saving change). Events RustCut makes are tagged so the window can list and delete
//! just those.
//! Uses the `calendar.events` scope on the primary calendar; sign-in is in `crate::google`.

use crate::google::{self, enc, SCOPE_CALENDAR};
use crate::localtime::{self, LocalTime};
use anyhow::{anyhow, bail, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

const API: &str = "https://www.googleapis.com/calendar/v3/calendars/primary/events";
/// Marks the events RustCut made (a private extended property).
const TAG: &str = "rustcut";

/// Where the content goes, with the calendar colour its events get.
pub const PLATFORMS: &[(&str, &str)] = &[
    ("YouTube", "11"),        // tomato
    ("YouTube Shorts", "4"),  // flamingo
    ("Instagram", "3"),       // grape
    ("TikTok", "8"),          // graphite
    ("Facebook", "9"),        // blueberry
    ("X", "1"),               // lavender
    ("Release / premiere", "5"), // banana
    ("Other", "7"),           // peacock
];

pub const REMINDERS: &[(Option<u32>, &str)] = &[(None, "None"), (Some(10), "10 minutes before"), (Some(30), "30 minutes before"), (Some(60), "1 hour before"), (Some(1440), "1 day before")];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Repeat {
    Once,
    Daily,
    Weekly,
}

impl Repeat {
    pub const ALL: [Repeat; 3] = [Repeat::Once, Repeat::Daily, Repeat::Weekly];
    pub fn label(self) -> &'static str {
        match self {
            Repeat::Once => "Once",
            Repeat::Daily => "Every day",
            Repeat::Weekly => "Every week",
        }
    }
}

/// One scheduled piece of content.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub title: String,
    pub platform: String,
    /// Local time as typed ("2026-11-25 18:00").
    pub start: String,
    pub minutes: u32,
    pub repeat: Repeat,
    /// How many times, when repeating.
    pub times: u32,
    pub reminder: Option<u32>,
    /// A link to the video (YouTube, Drive…).
    pub link: String,
    pub notes: String,
    /// The project it's for, kept on the event.
    pub project: String,
}

impl Default for Entry {
    fn default() -> Self {
        Self {
            title: String::new(),
            platform: "YouTube".into(),
            start: String::new(),
            minutes: 30,
            repeat: Repeat::Once,
            times: 4,
            reminder: Some(30),
            link: String::new(),
            notes: String::new(),
            project: String::new(),
        }
    }
}

pub fn color_for(platform: &str) -> &'static str {
    PLATFORMS.iter().find(|(p, _)| *p == platform).map(|(_, c)| *c).unwrap_or("7")
}

/// What's wrong with the entry, if anything.
pub fn validate(e: &Entry, now: i64) -> Vec<String> {
    let mut out = Vec::new();
    if e.title.trim().is_empty() {
        out.push("Say what's going out (the event's title).".to_string());
    }
    match LocalTime::parse(&e.start) {
        Err(err) => out.push(format!("When: {err}.")),
        Ok(t) if t.to_unix() < now - 60 => out.push("That time has already passed.".into()),
        Ok(_) => {}
    }
    if !(5..=24 * 60).contains(&e.minutes) {
        out.push("The length must be between 5 minutes and 24 hours.".into());
    }
    if e.repeat != Repeat::Once && !(2..=52).contains(&e.times) {
        out.push("Repeat 2 to 52 times.".into());
    }
    out
}

/// The local start time of each occurrence.
pub fn occurrences(e: &Entry) -> Result<Vec<LocalTime>> {
    let first = LocalTime::parse(&e.start)?;
    let (n, step) = match e.repeat {
        Repeat::Once => (1, 0),
        Repeat::Daily => (e.times.max(1), 1),
        Repeat::Weekly => (e.times.max(1), 7),
    };
    Ok((0..n as i64).map(|i| first.plus_days(i * step)).collect())
}

/// The Calendar API event body for the occurrence starting at `at`.
pub fn resource(e: &Entry, at: &LocalTime) -> Result<Value> {
    let start = at.to_unix();
    let end = start + e.minutes as i64 * 60;
    let mut description = String::new();
    if !e.notes.trim().is_empty() {
        description.push_str(e.notes.trim());
        description.push_str("\n\n");
    }
    if !e.link.trim().is_empty() {
        description.push_str(e.link.trim());
        description.push_str("\n\n");
    }
    description.push_str(&format!("Platform: {}", e.platform));
    if !e.project.is_empty() {
        description.push_str(&format!("\nProject: {}", e.project));
    }
    description.push_str("\nScheduled from RustCut");
    let reminders = match e.reminder {
        Some(m) => json!({ "useDefault": false, "overrides": [{ "method": "popup", "minutes": m }] }),
        None => json!({ "useDefault": false, "overrides": [] }),
    };
    let mut v = json!({
        "summary": e.title.trim(),
        "description": description,
        "start": { "dateTime": localtime::rfc3339_local(start) },
        "end": { "dateTime": localtime::rfc3339_local(end) },
        "colorId": color_for(&e.platform),
        "reminders": reminders,
        "extendedProperties": { "private": { TAG: "1", "platform": e.platform, "project": e.project } },
    });
    if !e.link.trim().is_empty() {
        v["source"] = json!({ "title": e.platform, "url": e.link.trim() });
    }
    Ok(v)
}

/// An event on the calendar that RustCut made.
#[derive(Clone, Debug, PartialEq)]
pub struct Scheduled {
    pub id: String,
    pub title: String,
    pub platform: String,
    pub start: i64,
    pub link: String,
}

fn parse_event(v: &Value) -> Option<Scheduled> {
    let start = v["start"]["dateTime"].as_str().or_else(|| v["start"]["date"].as_str())?;
    Some(Scheduled {
        id: v["id"].as_str()?.to_string(),
        title: v["summary"].as_str().unwrap_or("(no title)").to_string(),
        platform: v["extendedProperties"]["private"]["platform"].as_str().unwrap_or("").to_string(),
        start: localtime::parse_rfc3339(start)?,
        link: v["htmlLink"].as_str().unwrap_or_default().to_string(),
    })
}

fn token() -> Result<String> {
    let a = google::load();
    if !a.can(SCOPE_CALENDAR) {
        bail!("connect Google Calendar first (File > 📅 Schedule content…)");
    }
    google::access_token(&a)
}

/// Adds the entry (every occurrence) to the primary calendar and returns the events.
pub fn create(e: &Entry) -> Result<Vec<Scheduled>> {
    let problems = validate(e, localtime::now_unix());
    if !problems.is_empty() {
        bail!("{}", problems.join(" "));
    }
    let token = token()?;
    let mut made = Vec::new();
    for at in occurrences(e)? {
        let r = google::agent().post(API).set("Authorization", &format!("Bearer {token}")).send_json(resource(e, &at)?);
        match r {
            Ok(resp) => made.push(parse_event(&resp.into_json()?).ok_or_else(|| anyhow!("Google Calendar didn't return the new event"))?),
            Err(err) if made.is_empty() => return Err(google::api_error(err)),
            Err(err) => bail!("added {} of the events, then: {:#}", made.len(), google::api_error(err)),
        }
    }
    Ok(made)
}

/// RustCut's upcoming events, soonest first.
pub fn upcoming(max: usize) -> Result<Vec<Scheduled>> {
    let url = format!(
        "{API}?privateExtendedProperty={}&timeMin={}&singleEvents=true&orderBy=startTime&maxResults={max}",
        enc(&format!("{TAG}=1")),
        enc(&localtime::rfc3339_utc(localtime::now_unix()))
    );
    let v: Value = google::agent().get(&url).set("Authorization", &format!("Bearer {}", token()?)).call().map_err(google::api_error)?.into_json()?;
    Ok(v["items"].as_array().map(|a| a.iter().filter_map(parse_event).collect()).unwrap_or_default())
}

/// Removes an event.
pub fn delete(id: &str) -> Result<()> {
    match google::agent().delete(&format!("{API}/{}", enc(id))).set("Authorization", &format!("Bearer {}", token()?)).call() {
        Ok(_) | Err(ureq::Error::Status(410, _)) => Ok(()),
        Err(e) => Err(google::api_error(e)),
    }
}

/// True when the saved account may add calendar events.
pub fn ready() -> bool {
    google::load().can(SCOPE_CALENDAR)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry() -> Entry {
        Entry { title: "Post the Falling Deeper teaser".into(), platform: "TikTok".into(), start: "2030-03-01 18:00".into(), ..Default::default() }
    }

    #[test]
    fn validation() {
        let now = 1_795_000_000;
        assert!(validate(&entry(), now).is_empty());
        let mut e = entry();
        e.title = " ".into();
        e.start = "2020-01-01 10:00".into();
        e.minutes = 2;
        assert_eq!(validate(&e, now).len(), 3);
        e = entry();
        e.repeat = Repeat::Weekly;
        e.times = 1;
        assert!(validate(&e, now)[0].contains("Repeat"));
        e.start = "soon".into();
        assert!(validate(&e, now)[0].starts_with("When"));
    }

    #[test]
    fn event_body() {
        let mut e = entry();
        e.link = "https://youtu.be/abc".into();
        e.notes = "Use the chorus clip".into();
        e.project = "Falling Deeper".into();
        e.reminder = Some(60);
        e.repeat = Repeat::Weekly;
        e.times = 4;
        let when = occurrences(&e).unwrap();
        assert_eq!(when.iter().map(|t| t.to_input()).collect::<Vec<_>>(), ["2030-03-01 18:00", "2030-03-08 18:00", "2030-03-15 18:00", "2030-03-22 18:00"]);
        let v = resource(&e, &when[0]).unwrap();
        assert_eq!(v["summary"], "Post the Falling Deeper teaser");
        assert_eq!(v["colorId"], "8");
        assert_eq!(v["reminders"]["overrides"][0]["minutes"], 60);
        assert_eq!(v["extendedProperties"]["private"]["rustcut"], "1");
        assert_eq!(v["source"]["url"], "https://youtu.be/abc");
        let desc = v["description"].as_str().unwrap();
        assert!(desc.starts_with("Use the chorus clip\n\nhttps://youtu.be/abc") && desc.contains("Project: Falling Deeper"));
        let start = localtime::parse_rfc3339(v["start"]["dateTime"].as_str().unwrap()).unwrap();
        let end = localtime::parse_rfc3339(v["end"]["dateTime"].as_str().unwrap()).unwrap();
        assert_eq!(end - start, 30 * 60);
        assert_eq!(LocalTime::from_unix(start).to_input(), "2030-03-01 18:00");
        e.repeat = Repeat::Once;
        e.reminder = None;
        e.link.clear();
        assert_eq!(occurrences(&e).unwrap().len(), 1);
        let v = resource(&e, &occurrences(&e).unwrap()[0]).unwrap();
        assert!(v.get("source").is_none());
        assert_eq!(v["reminders"]["overrides"], json!([]));
    }

    #[test]
    fn listed_events() {
        let v = json!({ "id": "e1", "summary": "Premiere", "htmlLink": "https://calendar.google.com/x",
            "start": { "dateTime": "2030-03-01T18:00:00-05:00" }, "extendedProperties": { "private": { "platform": "YouTube" } } });
        let s = parse_event(&v).unwrap();
        assert_eq!((s.id.as_str(), s.platform.as_str()), ("e1", "YouTube"));
        assert_eq!(s.start, localtime::parse_rfc3339("2030-03-01T23:00:00Z").unwrap());
        assert!(parse_event(&json!({ "id": "x" })).is_none());
    }
}
