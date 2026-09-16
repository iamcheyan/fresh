//! tmux.
//!
//! # What there is to reverse-engineer
//!
//! Almost nothing on disk, and that is the finding. tmux keeps its session
//! model entirely in the server process; the only artefact in the filesystem
//! is the socket the server listens on, at `$TMUX_TMPDIR/tmux-<uid>/<name>`
//! (`/tmp` when `TMUX_TMPDIR` is unset). Confirmed on tmux 3.4:
//!
//! ```text
//! $ ls -la /tmp/tmux-0/
//! srw-rw---- 1 root root 0 Sep 16 05:58 default
//! ```
//!
//! So there are two discovery modes, and they answer different questions:
//!
//! * **Sockets only** (the default): "a tmux server exists, or recently did".
//!   A socket file outlives a crashed server, so its presence is
//!   [`Confidence::Possible`] and nothing more.
//! * **With commands enabled**: ask the server. `tmux -S <socket>
//!   list-sessions -F <format>` gives the real list, and `list-panes -a` the
//!   panes, working directories and foreground commands — which is where the
//!   agents actually are.
//!
//! # Finding agents in panes
//!
//! tmux records no agent identity, so a pane hosting Claude Code looks exactly
//! like a pane hosting `less`. The only signal is `pane_current_command`, the
//! basename of the foreground process. Matching it against
//! [`AgentId::from_command`] is a guess — a shell script named `claude` would
//! match too — so pane-derived agents never claim better than
//! [`Confidence::Possible`], and [`crate::model::Discovery::correlate`] is
//! what upgrades them by matching a pane against a real transcript.
//!
//! # Delimiting the fields, and tmux's output escaping
//!
//! Every field tmux can print may legitimately contain `|`, `:` or a space —
//! window names and pane titles are user-set, and working directories can
//! contain nearly anything — so the format strings here delimit with `\x1f`
//! (ASCII unit separator), which no path or title realistically holds.
//!
//! tmux does not pass that byte through. It escapes its format output, and a
//! first version of this provider silently returned zero sessions because of
//! it. Measured on tmux 3.4:
//!
//! ```text
//! $ tmux list-windows -F "$(printf 'X\037Y')" | od -c
//! 0000000   X   \   0   3   7   Y  \n          # the delimiter came back as four chars
//!
//! $ tmux rename-window 'a\b'; tmux list-windows -F '#{window_name}' | od -c
//! 0000000   a   \   \   b  \n                  # a literal backslash is doubled
//! ```
//!
//! So tmux applies C-style escaping: a control byte becomes `\nnn` octal, and a
//! literal backslash becomes `\\`. That is *reversible*, which is the good
//! news — but only with a scanner, not a substring split. A pane title
//! containing the four characters `\037` arrives as `\\037`, in which a naive
//! `split("\\037")` finds a false delimiter one character in.
//! [`split_escaped_fields`] therefore consumes each escape atomically, and is
//! the only place that decides what is a separator.
//!
//! The same escaping is why a record can be assumed to be one line: a newline
//! inside a window name comes back as `\012`, not as a line break.

use std::path::PathBuf;

use crate::env::Env;
use crate::model::{
    AgentId, Confidence, Evidence, Installed, MuxSession, Pane, ToolId, ToolReport,
};

/// Field separator for our `-F` format strings. See the module docs — tmux
/// emits this byte back as the four characters `\037`, so nothing splits on it
/// directly; [`split_escaped_fields`] is what understands the wire form.
#[allow(
    dead_code,
    reason = "documents the byte the format strings below request; \
                                     the wire form is what SEP_BYTE decodes"
)]
const SEP: char = '\u{1f}';

/// The byte value of [`SEP`], as tmux's octal escape encodes it.
const SEP_BYTE: u8 = 0x1f;

/// Split one line of tmux format output into its fields, undoing tmux's
/// escaping as it goes.
///
/// Escapes are consumed atomically, so a field whose *text* is `\037` (which
/// tmux sends as `\\037`) cannot be mistaken for a separator. An escape tmux
/// would never produce — a trailing lone backslash, `\9` — is passed through
/// as written rather than dropped, on the grounds that data we do not
/// understand is better reported verbatim than silently altered.
pub fn split_escaped_fields(line: &str) -> Vec<String> {
    let bytes = line.as_bytes();
    let mut fields = Vec::new();
    let mut current = Vec::new();
    let mut index = 0;

    while index < bytes.len() {
        if bytes[index] != b'\\' {
            current.push(bytes[index]);
            index += 1;
            continue;
        }
        match bytes.get(index + 1) {
            // An escaped backslash: one literal backslash, never a separator.
            Some(b'\\') => {
                current.push(b'\\');
                index += 2;
            }
            // `\nnn` octal.
            Some(digit) if digit.is_ascii_digit() => {
                let octal = &bytes[index + 1..(index + 4).min(bytes.len())];
                match decode_octal(octal) {
                    Some(value) => {
                        if value == SEP_BYTE {
                            fields.push(take_field(&mut current));
                        } else {
                            current.push(value);
                        }
                        index += 4;
                    }
                    None => {
                        current.push(bytes[index]);
                        index += 1;
                    }
                }
            }
            _ => {
                current.push(bytes[index]);
                index += 1;
            }
        }
    }
    fields.push(take_field(&mut current));
    fields
}

fn take_field(current: &mut Vec<u8>) -> String {
    let field = String::from_utf8_lossy(current).into_owned();
    current.clear();
    field
}

/// Three octal digits as a byte, or `None` if they are not three octal digits
/// or do not fit in one.
fn decode_octal(digits: &[u8]) -> Option<u8> {
    if digits.len() != 3 {
        return None;
    }
    let mut value: u16 = 0;
    for digit in digits {
        if !(b'0'..=b'7').contains(digit) {
            return None;
        }
        value = value * 8 + u16::from(digit - b'0');
    }
    u8::try_from(value).ok()
}

const SESSION_FORMAT: &str = "#{session_id}\u{1f}#{session_name}\u{1f}#{session_created}\u{1f}#{session_attached}\u{1f}#{session_path}";
const PANE_FORMAT: &str = "#{session_id}\u{1f}#{window_index}\u{1f}#{window_name}\u{1f}#{pane_id}\u{1f}#{pane_pid}\u{1f}#{pane_current_path}\u{1f}#{pane_current_command}\u{1f}#{pane_title}";

pub fn scan(env: &Env) -> ToolReport {
    let mut report = ToolReport::absent(ToolId::Tmux, "tmux");
    let sockets = socket_paths(env, &mut report);

    if let Some(path) = env.which("tmux") {
        report.installed = Installed::Present {
            version: env
                .run("tmux", &["-V"])
                .ok()
                .map(|out| out.trim().to_string()),
            executable: Some(path.clone()),
        };
        report.evidence.push(Evidence::executable(path));
    } else if !sockets.is_empty() {
        // A socket without a binary on our PATH still means tmux is here; the
        // binary may simply not be on the scanning process's PATH.
        report.installed = Installed::Present {
            version: None,
            executable: None,
        };
    }

    for socket in sockets {
        report.evidence.push(
            Evidence::socket(&socket).saying("tmux server socket (may outlive a crashed server)"),
        );
        match live_sessions(env, &socket) {
            Ok(sessions) => report.mux_sessions.extend(sessions),
            Err(err) => {
                // Falling back is not a failure: without commands enabled, the
                // socket alone is the honest answer.
                if env.commands_enabled() {
                    report.note_problem(err);
                }
                report.mux_sessions.push(MuxSession {
                    id: socket.to_string_lossy().into_owned(),
                    name: socket
                        .file_name()
                        .and_then(|name| name.to_str())
                        .map(str::to_string),
                    created_unix: None,
                    attached: None,
                    socket: Some(socket.clone()),
                    panes: Vec::new(),
                    confidence: Confidence::Possible,
                    evidence: vec![Evidence::socket(&socket)
                        .saying("socket exists; server not queried, so session list is unknown")],
                });
            }
        }
    }

    report
}

/// Every tmux socket we can see.
///
/// tmux puts them in `$TMUX_TMPDIR/tmux-<uid>/`, defaulting `TMUX_TMPDIR` to
/// `/tmp`. The directory is mode 0700, so on a shared machine we see only our
/// own — which is the correct scope for this crate anyway.
fn socket_paths(env: &Env, report: &mut ToolReport) -> Vec<PathBuf> {
    let tmpdir = env
        .var("TMUX_TMPDIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    if env.var("TMUX_TMPDIR").is_some() {
        report
            .evidence
            .push(Evidence::env_var("TMUX_TMPDIR", tmpdir.to_string_lossy()));
    }
    let dir = tmpdir.join(format!("tmux-{}", env.uid()));
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    report.evidence.push(Evidence::dir(&dir));
    let mut sockets: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| is_socket(path))
        .collect();
    sockets.sort();
    sockets
}

#[cfg(unix)]
fn is_socket(path: &std::path::Path) -> bool {
    use std::os::unix::fs::FileTypeExt;
    std::fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_socket())
}

#[cfg(not(unix))]
fn is_socket(_path: &std::path::Path) -> bool {
    false
}

/// Ask the server on `socket` for its sessions and panes.
fn live_sessions(env: &Env, socket: &std::path::Path) -> Result<Vec<MuxSession>, String> {
    let socket_arg = socket.to_str().ok_or("socket path is not utf-8")?;
    let listed = env.run(
        "tmux",
        &["-S", socket_arg, "list-sessions", "-F", SESSION_FORMAT],
    )?;

    // Panes come from one `list-panes -a` rather than one call per session:
    // the server is being asked about state that can change between calls, and
    // a single snapshot at least cannot disagree with itself.
    let panes_by_session = match env.run(
        "tmux",
        &["-S", socket_arg, "list-panes", "-a", "-F", PANE_FORMAT],
    ) {
        Ok(output) => parse_panes(&output),
        Err(err) => {
            tracing::debug!(%err, "tmux list-panes failed; reporting sessions without panes");
            Vec::new()
        }
    };

    let mut sessions = Vec::new();
    for line in listed.lines().filter(|line| !line.trim().is_empty()) {
        let fields = split_escaped_fields(line);
        let [id, name, created, attached, path] = fields.as_slice() else {
            tracing::debug!(line, "unexpected tmux list-sessions field count; skipping");
            continue;
        };
        let panes = panes_by_session
            .iter()
            .filter(|(session_id, _)| session_id == id)
            .map(|(_, pane)| pane.clone())
            .collect();
        sessions.push(MuxSession {
            id: (*id).to_string(),
            name: Some((*name).to_string()),
            created_unix: created.parse().ok(),
            // tmux prints the number of attached clients, not a boolean.
            attached: attached.parse::<u32>().ok().map(|count| count > 0),
            socket: Some(socket.to_path_buf()),
            panes,
            confidence: Confidence::Certain,
            evidence: vec![
                Evidence::socket(socket),
                Evidence::command(format!("tmux -S {socket_arg} list-sessions"))
                    .saying(format!("session at {path}")),
            ],
        });
    }
    Ok(sessions)
}

/// `(session_id, pane)` for each line of `list-panes -a` output.
fn parse_panes(output: &str) -> Vec<(String, Pane)> {
    output
        .lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| {
            let fields = split_escaped_fields(line);
            let [session_id, window_index, window_name, pane_id, pid, cwd, command, title] =
                fields.as_slice()
            else {
                tracing::debug!(line, "unexpected tmux list-panes field count; skipping");
                return None;
            };
            Some((
                (*session_id).to_string(),
                Pane {
                    id: (*pane_id).to_string(),
                    window: Some(format!("{window_index}:{window_name}")),
                    pid: pid.parse().ok(),
                    cwd: Some(PathBuf::from(cwd)).filter(|p| !p.as_os_str().is_empty()),
                    command: Some((*command).to_string()),
                    title: Some((*title).to_string()).filter(|t| !t.is_empty()),
                    agent: AgentId::from_command(command),
                    // tmux records no agent session id. Correlation fills this
                    // gap by matching cwd + agent against real transcripts.
                    agent_session_id: None,
                },
            ))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::env::StubRunner;

    /// Captured from tmux 3.4 with the format strings above — in the *wire*
    /// form tmux actually emits, where the `\x1f` separator comes back as the
    /// four characters `\037`. Writing these as raw `\u{1f}` bytes is what the
    /// first version of this provider got wrong, and it returned zero sessions
    /// against a live server while its tests passed.
    const SESSIONS: &str = r"$0\037probe\0371789538289\0370\037/home/user/fresh
";
    const PANES: &str = r"$0\0370\037bash\037%0\0371896\037/home/user/fresh\037claude\037vm
$0\0371\037agentwin\037%1\0371901\037/tmp\037bash\037vm
";

    fn stub(socket: &str) -> StubRunner {
        StubRunner::new()
            .returning(
                "tmux",
                &["-S", socket, "list-sessions", "-F", SESSION_FORMAT],
                SESSIONS,
            )
            .returning(
                "tmux",
                &["-S", socket, "list-panes", "-a", "-F", PANE_FORMAT],
                PANES,
            )
    }

    #[test]
    fn parses_a_real_session_listing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let socket = dir.path().join("default");
        let socket_arg = socket.to_str().expect("utf-8");
        let env = Env::fake(dir.path()).with_commands(stub(socket_arg));

        let sessions = live_sessions(&env, &socket).expect("listing");

        assert_eq!(sessions.len(), 1);
        let session = &sessions[0];
        assert_eq!(session.id, "$0");
        assert_eq!(session.name.as_deref(), Some("probe"));
        assert_eq!(session.created_unix, Some(1_789_538_289));
        assert_eq!(session.attached, Some(false));
        assert_eq!(session.panes.len(), 2);
    }

    #[test]
    fn recognises_an_agent_from_the_foreground_command() {
        let panes = parse_panes(PANES);
        assert_eq!(panes[0].1.agent, Some(AgentId::Claude));
        assert_eq!(
            panes[0].1.cwd.as_deref(),
            Some(std::path::Path::new("/home/user/fresh"))
        );
        assert_eq!(panes[1].1.agent, None, "a plain shell is not an agent");
    }

    #[test]
    fn a_pane_title_spelled_like_the_escape_is_not_a_separator() {
        // tmux sends a literal backslash doubled, so a title whose text is
        // `x\037y` arrives as `x\\037y`. A substring split finds a false
        // separator one character in and shifts every later field.
        let line = r"$0\0370\037w\037%0\0371\037/tmp\037bash\037x\\037y";
        let panes = parse_panes(line);
        assert_eq!(panes.len(), 1, "the row must survive intact");
        assert_eq!(
            panes[0].1.title.as_deref(),
            Some(r"x\037y"),
            "the escaped backslash must decode back to one backslash"
        );
    }

    #[test]
    fn a_control_byte_in_a_field_decodes_rather_than_splitting() {
        // A window name containing a tab (`\011`) is data, not a separator.
        let line = r"$0\0370\037a\011b\037%0\0371\037/tmp\037bash\037t";
        let panes = parse_panes(line);
        assert_eq!(panes.len(), 1);
        assert_eq!(panes[0].1.window.as_deref(), Some("0:a\tb"));
    }

    #[test]
    fn a_genuinely_short_row_is_skipped_not_misparsed() {
        assert!(parse_panes(r"$0\0370\037only-three").is_empty());
    }

    #[test]
    fn octal_decoding_rejects_what_tmux_would_never_send() {
        assert_eq!(decode_octal(b"037"), Some(0x1f));
        assert_eq!(decode_octal(b"012"), Some(b'\n'));
        assert_eq!(decode_octal(b"08"), None, "too short, and 8 is not octal");
        assert_eq!(decode_octal(b"777"), None, "511 does not fit in a byte");
    }

    #[test]
    fn a_lone_trailing_backslash_is_passed_through_not_dropped() {
        assert_eq!(split_escaped_fields(r"a\"), vec![r"a\".to_string()]);
    }

    #[test]
    fn commands_are_not_run_unless_enabled() {
        let dir = tempfile::tempdir().expect("tempdir");
        let env = Env::fake(dir.path());
        let err = live_sessions(&env, &dir.path().join("default")).expect_err("must refuse");
        assert!(err.contains("disabled"), "unexpected error: {err}");
    }
}
