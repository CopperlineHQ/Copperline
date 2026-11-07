// SPDX-License-Identifier: GPL-3.0-or-later

//! The host side of the Toaster's ARexx link: how an MCP client gets an
//! ARexx script run inside the emulated Amiga and its output back.
//!
//! The Video Toaster's software is driven through ARexx: the switcher's
//! `ToasterARexx.port` (`Switcher(TAKE)`, `Switcher(TBAR, 256)` ...) and
//! the project editor's `PROJECT_REXX_PORT` (`CROUTONLOAD`,
//! `PROJECTSAVE` ...). Those ports live inside the guest, so the link is a
//! folder both sides can see: the card's host folder, mounted in the Amiga
//! with `[video_slot] volume` (`ToasterFiles:`), holds a `.copperline`
//! folder. The host drops a job there; `S:Copperline-Link.rexx`, started
//! by `S:User-Startup`, runs it with `RX` and writes its output and return
//! code back:
//!
//! - host writes `job-ID.rexx` (through a temporary name, so the guest
//!   never sees half a script);
//! - guest renames it `run-ID.rexx` (taken) and starts it in a process
//!   of its own as `RX >out-ID.txt run-ID.rexx`; when RX returns, the job
//!   is deleted and its return code written to `done-ID.txt` (also
//!   through a temporary name);
//! - host reads `done-ID.txt` and `out-ID.txt` and deletes both.
//!
//! The guest polls five times a second, so a job starts within about
//! 0.2 s of emulated time. Jobs run side by side, so one that never
//! returns (a call to a program that does not answer its port) costs the
//! caller a timeout but does not stop the link.

use std::path::{Path, PathBuf};

/// The guest-side script, installed as `S:Copperline-Link.rexx`.
pub const GUEST_SCRIPT: &str = include_str!("../../assets/toaster/Copperline-Link.rexx");

/// The finished job: `RX`'s return code and what the script printed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobResult {
    pub rc: i64,
    pub output: String,
}

/// Quote `s` as an ARexx string literal.
pub fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// An argument for a Toaster function: integers stay bare, anything
/// else is a quoted string.
pub fn arg(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::String(s)
            if !s.is_empty()
                && s.trim_start_matches('-')
                    .chars()
                    .all(|c| c.is_ascii_digit()) =>
        {
            s.clone()
        }
        serde_json::Value::String(s) => quote(s),
        serde_json::Value::Bool(b) => u8::from(*b).to_string(),
        other => quote(&other.to_string()),
    }
}

/// A call of ARexx function `function` with `args`, its result printed
/// (`OK` for a function that returns nothing, as some Toaster ones do).
pub fn call_body(function: &str, args: &[serde_json::Value]) -> String {
    let args: Vec<String> = args.iter().map(arg).collect();
    format!(
        "DROP result\n\
         call {function}({})\n\
         IF SYMBOL('RESULT') = 'VAR' THEN say result\n\
         ELSE say 'OK'\n",
        args.join(", ")
    )
}

/// `body` wrapped so it can use both Toaster ports and reports errors
/// instead of hanging on them.
///
/// ARexx's list of function hosts is shared by every script in the Amiga,
/// and the switcher's host answers every call put to it (and blocks while
/// the CG is up), so the two ports go on the list only for the job, as
/// NewTek's StartToaster does it: added behind the hosts other programs
/// add (priority -1), and taken off again when the job is done. Left on
/// the list they would take the calls of the CG's own macros -- its
/// start-up macro, which loads its fonts, and its Alt-F keys -- and hang
/// them.
pub fn wrap(body: &str) -> String {
    format!(
        "/* Copperline link job */\n\
         OPTIONS RESULTS\n\
         SIGNAL ON SYNTAX\n\
         IF ~SHOW('Libraries','rexxsupport.library') THEN call addlib('rexxsupport.library', 0, -30, 0)\n\
         IF SHOW('Libraries','CG_AREXX') THEN call remlib('CG_AREXX')\n\
         copperline_hosts = ''\n\
         IF SHOW('Ports','ToasterARexx.port') & ~SHOW('Libraries','ToasterARexx.port') THEN DO\n  \
         call addlib('ToasterARexx.port', -1); copperline_hosts = 'ToasterARexx.port'\n\
         END\n\
         IF SHOW('Ports','PROJECT_REXX_PORT') & ~SHOW('Libraries','PROJECT_REXX_PORT') THEN DO\n  \
         call addlib('PROJECT_REXX_PORT', -1); copperline_hosts = copperline_hosts 'PROJECT_REXX_PORT'\n\
         END\n\
         {body}\n\
         call copperline_done\n\
         exit 0\n\
         SYNTAX:\n\
         say 'ARexx error' rc 'on line' sigl':' errortext(rc)\n\
         call copperline_done\n\
         exit 20\n\
         copperline_done:\n\
         DO WHILE copperline_hosts ~= ''\n  \
         PARSE VAR copperline_hosts copperline_host copperline_hosts\n  \
         call remlib(copperline_host)\n\
         END\n\
         RETURN\n"
    )
}

/// A Toaster-suite program reached through its ARexx port.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct App {
    pub name: &'static str,
    /// The ARexx port.
    pub port: &'static str,
    /// A function host (`addlib` it and call functions) rather than a
    /// command host (`address` it and send command strings).
    pub function_host: bool,
    /// The switcher command that loads it, if the switcher loads it.
    pub load: Option<&'static str>,
}

pub const APPS: &[App] = &[
    App {
        name: "switcher",
        port: "ToasterARexx.port",
        function_host: true,
        load: None,
    },
    App {
        name: "project",
        port: "PROJECT_REXX_PORT",
        function_host: true,
        load: None,
    },
    App {
        name: "cg",
        port: "CG_AREXX",
        function_host: true,
        load: Some("LDCG"),
    },
    App {
        name: "paint",
        port: "DigiPaint",
        function_host: false,
        load: Some("LDTP"),
    },
    App {
        name: "lightwave",
        port: "LightWaveARexx.port",
        function_host: false,
        load: Some("LDLW"),
    },
    App {
        name: "modeler",
        port: "LWModelerARexx.port",
        function_host: true,
        load: None,
    },
];

pub const APP_NAMES: &str = "switcher, project, cg, paint, lightwave or modeler";

pub fn app(name: &str) -> Option<&'static App> {
    let name = name.to_ascii_lowercase();
    let name = match name.as_str() {
        "toastercg" | "cg" => "cg",
        "toasterpaint" | "paint" => "paint",
        "lw" | "lightwave" => "lightwave",
        "editor" | "flyer" | "project" => "project",
        other => other,
    };
    APPS.iter().find(|a| a.name == name)
}

/// The ARexx for `command` on `app`.
pub fn app_body(app: &App, command: &str, args: &[serde_json::Value]) -> Result<String, String> {
    if command.eq_ignore_ascii_case("open") {
        let Some(load) = app.load else {
            return Err(format!(
                "{} is opened from the Toaster's own interface",
                app.name
            ));
        };
        let mut body = format!(
            "IF ~SHOW('Libraries','ToasterARexx.port') THEN call addlib('ToasterARexx.port', 0)\n\
             say 'load' Switcher('{load}')\n"
        );
        if app.name == "paint" {
            // STTP only returns when Paint is left: run it on its own,
            // with the switcher's host on the list for that call alone
            // (this job's own comes off first; see `wrap`).
            body.push_str(
                "IF OPEN('s', 'T:copperline-open.rexx', 'W') THEN DO\n  \
                 call WRITELN('s', \"call addlib('ToasterARexx.port', -1)\")\n  \
                 call WRITELN('s', \"call Switcher('STTP')\")\n  \
                 call WRITELN('s', \"call remlib('ToasterARexx.port')\")\n  \
                 call CLOSE('s')\n  \
                 call copperline_done\n  \
                 address command 'Run >NIL: SYS:Rexxc/RX T:copperline-open.rexx'\n  \
                 say 'entering ToasterPaint'\n\
                 END\n",
            );
        }
        return Ok(body);
    }
    if command.is_empty()
        || !command
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.')
    {
        return Err(format!("command must be a plain name, got {command:?}"));
    }
    let missing = format!(
        "IF ~SHOW('Ports','{port}') THEN DO\n  \
         say 'The {name} port ({port}) is not open: start {name} first'\n  \
         exit 5\n\
         END\n",
        port = app.port,
        name = app.name
    );
    if app.function_host {
        // ARexx asks the function hosts in turn and the switcher's host
        // answers every call (with an error for one it does not know), so
        // another program's host goes on the list alone for its call, and
        // comes off again: the CG's host blocks while the CG is off screen.
        let own = app.port == "ToasterARexx.port" || app.port == "PROJECT_REXX_PORT";
        let (alone, after) = if own {
            (String::new(), String::new())
        } else {
            (
                "call remlib('ToasterARexx.port'); call remlib('PROJECT_REXX_PORT')\n".to_string(),
                format!("call remlib('{}')\n", app.port),
            )
        };
        Ok(format!(
            "{missing}{alone}IF ~SHOW('Libraries','{port}') THEN call addlib('{port}', 0)\n{}{after}",
            call_body(command, args),
            port = app.port
        ))
    } else {
        let words: Vec<String> = std::iter::once(command.to_string())
            .chain(args.iter().map(|a| match a {
                serde_json::Value::String(s) if s.contains(' ') || s.is_empty() => {
                    format!("\"{s}\"")
                }
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            }))
            .collect();
        Ok(format!(
            "{missing}ADDRESS '{port}'\n{}\nsay 'rc' rc\nIF symbol('RESULT') = 'VAR' THEN say result\n",
            quote(&words.join(" ")),
            port = app.port
        ))
    }
}

fn job_name(id: u64) -> String {
    format!("job-{id}.rexx")
}

fn out_name(id: u64) -> String {
    format!("out-{id}.txt")
}

fn done_name(id: u64) -> String {
    format!("done-{id}.txt")
}

/// Hand `script` to the guest as job `id`.
pub fn submit(dir: &Path, id: u64, script: &str) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join(format!("job-{id}.tmp"));
    // Amiga line ends; the guest reads the script as it is.
    std::fs::write(&tmp, script.replace("\r\n", "\n"))?;
    std::fs::rename(&tmp, dir.join(job_name(id)))
}

/// Job `id`'s result once the guest has finished it; the files are
/// removed.
pub fn collect(dir: &Path, id: u64) -> Option<JobResult> {
    let done = dir.join(done_name(id));
    let rc_text = std::fs::read_to_string(&done).ok()?;
    let out = dir.join(out_name(id));
    let output = std::fs::read(&out)
        .map(|bytes| String::from_utf8_lossy(&bytes).replace('\r', ""))
        .unwrap_or_default();
    let _ = std::fs::remove_file(&done);
    let _ = std::fs::remove_file(&out);
    Some(JobResult {
        rc: rc_text.trim().parse().unwrap_or(-1),
        output: output.trim_end().to_string(),
    })
}

/// Take back a job the guest never picked up (on a timeout), so it does
/// not run later by surprise. `false` when the guest already has it.
pub fn withdraw(dir: &Path, id: u64) -> bool {
    std::fs::remove_file(dir.join(job_name(id))).is_ok()
}

/// A job id unlikely to collide with one left over from an earlier
/// bridge: the clock's milliseconds, then counting up.
pub fn first_id() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64 % 1_000_000_000)
        .unwrap_or(1)
}

/// The link folder named in a `toaster.status` reply.
pub fn folder_from_status(status: &serde_json::Value) -> Option<PathBuf> {
    status
        .pointer("/state/link_folder")
        .and_then(serde_json::Value::as_str)
        .map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn arguments_are_quoted_unless_numbers() {
        assert_eq!(arg(&json!(256)), "256");
        assert_eq!(arg(&json!("-3")), "-3");
        assert_eq!(arg(&json!("TAKE")), "'TAKE'");
        assert_eq!(arg(&json!("it's")), "'it''s'");
        let body = call_body("Switcher", &[json!("TBAR"), json!(256)]);
        assert!(body.contains("call Switcher('TBAR', 256)\n"));
        assert!(body.contains("ELSE say 'OK'"));
        assert!(call_body("CROUTONSINPROJECT", &[]).contains("call CROUTONSINPROJECT()\n"));
    }

    #[test]
    fn programs_are_reached_through_their_ports() {
        let cg = app("ToasterCG").unwrap();
        let body = app_body(cg, "GET_PAGE", &[]).unwrap();
        assert!(body.contains("addlib('CG_AREXX', 0)"));
        assert!(body.contains("call GET_PAGE()"));
        let lw = app("lightwave").unwrap();
        let body = app_body(lw, "LoadScene", &[json!("Toaster:LW/Scenes/a b")]).unwrap();
        assert!(body.contains("ADDRESS 'LightWaveARexx.port'"));
        assert!(body.contains("'LoadScene \"Toaster:LW/Scenes/a b\"'"));
        let paint = app("paint").unwrap();
        assert!(app_body(paint, "open", &[]).unwrap().contains("STTP"));
        assert!(app_body(app("modeler").unwrap(), "open", &[]).is_err());
        assert!(app_body(cg, "x; y", &[]).is_err());
        assert!(app("photoshop").is_none());
    }

    #[test]
    fn a_job_round_trips_through_the_folder() {
        let dir = tempfile::tempdir().unwrap();
        let link = dir.path().join(".copperline");
        submit(&link, 7, &wrap("say 1+1")).unwrap();
        let job = std::fs::read_to_string(link.join("job-7.rexx")).unwrap();
        assert!(job.contains("say 1+1"));
        assert!(!link.join("job-7.tmp").exists());
        assert_eq!(collect(&link, 7), None);
        // What the guest does.
        std::fs::remove_file(link.join("job-7.rexx")).unwrap();
        std::fs::write(link.join("out-7.txt"), "2\n").unwrap();
        std::fs::write(link.join("done-7.txt"), "0\n").unwrap();
        assert_eq!(
            collect(&link, 7),
            Some(JobResult {
                rc: 0,
                output: "2".into()
            })
        );
        assert!(std::fs::read_dir(&link).unwrap().next().is_none());
    }

    /// The switcher's hosts are on ARexx's shared function-host list only
    /// while a job runs, behind any host added at the usual priority, and
    /// come off on the way out, error or not: left there they take the
    /// CG's own macros' calls.
    #[test]
    fn a_job_takes_the_toaster_hosts_off_the_list_when_done() {
        let job = wrap("say 1+1");
        for port in ["ToasterARexx.port", "PROJECT_REXX_PORT"] {
            assert!(job.contains(&format!("call addlib('{port}', -1)")));
            assert!(!job.contains(&format!("call addlib('{port}', 0)")));
        }
        let body = job.find("say 1+1").unwrap();
        let done = job.find("copperline_done:").unwrap();
        // Called after the body and from the error handler, defined last.
        assert_eq!(job.matches("call copperline_done\n").count(), 2);
        assert!(job[body..done].contains("SYNTAX:"));
        assert!(job[done..].contains("call remlib(copperline_host)"));
        assert!(job.trim_end().ends_with("RETURN"));
        // Paint's own call, made after this job ends, adds its host itself.
        let paint = app_body(app("paint").unwrap(), "open", &[]).unwrap();
        let off = paint.find("call copperline_done").unwrap();
        assert!(off < paint.find("Run >NIL:").unwrap());
        assert!(paint.contains("call remlib('ToasterARexx.port')"));
    }

    #[test]
    fn a_job_not_yet_taken_can_be_withdrawn() {
        let dir = tempfile::tempdir().unwrap();
        submit(dir.path(), 3, "say 3").unwrap();
        assert!(withdraw(dir.path(), 3));
        assert!(!withdraw(dir.path(), 3));
    }

    #[test]
    fn the_guest_script_runs_jobs_from_the_link_folder() {
        assert!(GUEST_SCRIPT.contains("ToasterFiles:.copperline"));
        assert!(GUEST_SCRIPT.contains("JOB-"));
        assert!(GUEST_SCRIPT.contains("run-"));
        assert!(GUEST_SCRIPT.is_ascii());
    }
}
