//! One module per tool whose storage format we had to work out.
//!
//! Each provider is a plain `scan(&Env) -> ToolReport`: no trait object, no
//! registration. The set of tools is fixed at compile time and small enough to
//! list in [`crate::scan`], and a free function is far easier to test — every
//! provider's tests build a `tempfile::TempDir` laid out like a real install
//! and assert on the report.
//!
//! The rule every provider follows: **a scan never fails**. A missing
//! directory means the tool is not installed; an unreadable one, or a state
//! file that does not parse, is pushed onto `ToolReport::problems` and the scan
//! carries on. One tool updating its format must not take the other twelve
//! down with it.

pub mod claude_code;
pub mod claude_desktop;
pub mod codex;
pub mod herdr;
pub mod multiplexers;
pub mod orchestrators;
pub mod tmux;
