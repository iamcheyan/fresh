//! The Discover dialog scans through the agent-sessions plugin hub.
//!
//! Every scanner lives in a plugin now; the dialog opens a machine from a
//! window's own authority and asks the hub to run each registered scanner
//! against it. Nothing in core knows what a tmux socket or a Codex rollout is.
//!
//! This covers the whole chain end to end — palette command → dialog →
//! `getPluginApi("agent-sessions").scan()` → `openMachine` → the scanners →
//! rendered rows. It was worth writing because the chain was silently broken
//! once already: the scanners registered into a hub that nothing ever asked to
//! scan, because the dialog still called the Rust API that has since been
//! deleted.
//!
//! What a scan *finds* depends on whoever's home directory the test runs
//! against, so the assertions are about the dialog answering rather than about
//! any particular tool being installed.

use crate::common::harness::EditorTestHarness;
use crossterm::event::{KeyCode, KeyModifiers};

const WIDTH: u16 = 160;
const HEIGHT: u16 = 40;

fn run_palette(harness: &mut EditorTestHarness, command_name: &str) {
    harness
        .send_key(KeyCode::Char('p'), KeyModifiers::CONTROL)
        .unwrap();
    harness.render().unwrap();
    harness.type_text(command_name).unwrap();
    harness.render().unwrap();
    harness
        .send_key(KeyCode::Enter, KeyModifiers::NONE)
        .unwrap();
    harness.render().unwrap();
}

#[test]
fn discover_dialog_scans_through_the_plugin_hub() {
    let mut harness = EditorTestHarness::with_temp_project(WIDTH, HEIGHT).unwrap();

    run_palette(&mut harness, "Discover Agent Sessions");
    let opened = harness.screen_to_string();
    assert!(
        opened.contains("Discover agent sessions"),
        "the dialog opens; screen was:\n{opened}"
    );
    assert!(
        opened.contains("Machine"),
        "with the machine picker; screen was:\n{opened}"
    );
    // Before a scan there is a hint rather than an empty list: the dialog has
    // not looked at anything yet, and saying "no sessions" would be a lie.
    assert!(
        opened.contains("Pick a machine and choose Scan"),
        "and the pre-scan hint; screen was:\n{opened}"
    );

    // Focus starts on Scan, so Enter runs it. The scan is off the editor
    // thread, so tick until it settles.
    harness
        .send_key(KeyCode::Enter, KeyModifiers::NONE)
        .unwrap();
    let mut settled = String::new();
    for _ in 0..400 {
        harness.tick_and_render().unwrap();
        let screen = harness.screen_to_string();
        // Either the scan found nothing, or it rendered a tool group — both
        // mean it ran. The hint and the pending line mean it has not.
        if !screen.contains("Pick a machine and choose Scan")
            && !screen.contains("Scanning for agent sessions")
        {
            settled = screen;
            break;
        }
    }
    assert!(
        !settled.is_empty(),
        "the scan settled rather than hanging on the pending line"
    );
    assert!(
        settled.contains("Discover agent sessions"),
        "the dialog is still up with its answer; screen was:\n{settled}"
    );
    // The scan reached the hub and the hub reached the machine. This is the
    // assertion that bites: a missing hub, a rejected `openMachine` spec or a
    // throw anywhere in the chain all surface here instead of as rows.
    assert!(
        !settled.contains("Session discovery failed"),
        "the scan completed rather than failing; screen was:\n{settled}"
    );

    // Esc closes it without disturbing the editor.
    harness.send_key(KeyCode::Esc, KeyModifiers::NONE).unwrap();
    harness.render().unwrap();
    let closed = harness.screen_to_string();
    assert!(
        !closed.contains("Pick a machine and choose Scan"),
        "the dialog closed; screen was:\n{closed}"
    );
    assert!(!harness.editor().should_quit(), "and the editor is fine");
}
