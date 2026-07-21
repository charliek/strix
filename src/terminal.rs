use std::io::{self, Stdout, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use anyhow::Result;
use ratatui::backend::{CrosstermBackend, TestBackend};
use ratatui::buffer::Buffer;
use ratatui::crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    Event, KeyEventKind, KeyboardEnhancementFlags, PopKeyboardEnhancementFlags,
    PushKeyboardEnhancementFlags,
};
use ratatui::crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, supports_keyboard_enhancement, EnterAlternateScreen,
    LeaveAlternateScreen,
};
use ratatui::crossterm::{execute, queue};
use ratatui::Terminal;

use crate::app::App;
use crate::ui;

type Tui = Terminal<CrosstermBackend<Stdout>>;

/// xterm "any-event" mouse tracking: report pointer motion even with no button
/// held, so hovering the split bar can be detected. `EnableMouseCapture` only
/// reports motion while a button is down, so this is requested separately.
const ENABLE_MOUSE_MOTION: &str = "\x1b[?1003h";
const DISABLE_MOUSE_MOTION: &str = "\x1b[?1003l";
/// OSC 22: request a mouse pointer shape by CSS cursor name. Supported by
/// kitty, WezTerm, Alacritty, Ghostty, …; silently ignored elsewhere (e.g.
/// iTerm2). We use `pointer` (the hand) over the more literal `col-resize`: it's
/// the "grabbable" cue we want and one of the few shapes Ghostty honours on
/// macOS. The reset uses the explicit `default` name, not the empty-name form —
/// Ghostty doesn't act on the latter, so the cursor would otherwise stick.
const POINTER_GRAB: &str = "\x1b]22;pointer\x1b\\";
const POINTER_DEFAULT: &str = "\x1b]22;default\x1b\\";
/// How long the loop blocks for input before checking the file-watcher channel,
/// when a watcher is active. Bounds the latency to notice a filesystem change.
const POLL_TIMEOUT: Duration = Duration::from_millis(200);

/// Whether we pushed the keyboard-enhancement flag at setup, so `restore` pops
/// exactly what it pushed — and nothing at all on terminals that never got one.
static KBD_ENHANCED: AtomicBool = AtomicBool::new(false);

/// Set up the terminal, run the event loop, and restore the terminal on the way
/// out (including on panic, via the installed hook).
pub fn run(mut app: App, watch_rx: Option<Receiver<()>>) -> Result<()> {
    install_panic_hook();
    let mut terminal = setup()?;
    let result = event_loop(&mut terminal, &mut app, watch_rx);
    restore()?;
    result
}

fn setup() -> Result<Tui> {
    enable_raw_mode()?;
    // Once any mode is enabled, a later failure must undo it: `restore()` is only
    // reached on normal exit and the panic hook, so a `?` bail-out mid-setup would
    // otherwise leave the real terminal in raw / alt-screen / bracketed-paste mode
    // (codex fix #5). Best-effort restore, then propagate the original error.
    setup_modes().inspect_err(|_| {
        let _ = restore();
    })
}

/// Enable the alternate screen, mouse capture + motion, and bracketed paste, and
/// build the terminal. Split out so [`setup`] can clean up on any failure here.
fn setup_modes() -> Result<Tui> {
    let mut stdout = io::stdout();
    // Probe keyboard-enhancement support first — while still on the normal
    // screen and before mouse capture/motion. The query blocks up to ~2s on a
    // terminal that ignores it, so any stall is paid here (not against a frozen
    // blank alt-screen) and the query round-trip stays clean of interleaved
    // mouse reports.
    let enhanced = supports_keyboard_enhancement().unwrap_or(false);
    // Bracketed paste so a multi-line paste arrives as one `Event::Paste` (its
    // newlines insert into the comment editor) rather than a burst of Enter keys.
    execute!(
        stdout,
        EnterAlternateScreen,
        EnableMouseCapture,
        EnableBracketedPaste
    )?;
    write!(stdout, "{ENABLE_MOUSE_MOTION}")?;
    // Enable DISAMBIGUATE_ESCAPE_CODES so modified keys report distinctly —
    // notably Shift+Enter, which the comment editor maps to a newline but plain
    // terminals never report the modifier for. Best-effort: the flag is recorded
    // only after the push is written, so a failed push never leaves `restore`
    // popping a stack entry we never pushed; if it fails, enhancement just stays
    // off and the terminal is otherwise fully usable.
    let _ = push_kbd_enhancement(&mut stdout, enhanced, &KBD_ENHANCED);
    stdout.flush()?;
    Ok(Terminal::new(CrosstermBackend::new(stdout))?)
}

fn restore() -> Result<()> {
    let mut stdout = io::stdout();
    // Attempt every teardown step even if an earlier one fails, then return the
    // first error. A `?` mid-teardown would short-circuit the rest — a failed Pop
    // would skip LeaveAlternateScreen and disable_raw_mode, stranding the user on
    // the alternate screen in raw mode. Order mirrors setup in reverse: pop the
    // keyboard-enhancement flag first (before leaving the alt screen / raw mode),
    // and only if we actually pushed it.
    let pop = pop_kbd_enhancement(&mut stdout, &KBD_ENHANCED);
    // Undo the motion request and any custom pointer shape before leaving, so
    // the terminal isn't left reporting movement or showing a grab cursor.
    let pointer = write!(stdout, "{POINTER_DEFAULT}{DISABLE_MOUSE_MOTION}");
    // One `execute!` per command: a single `execute!` chains internally with
    // `and_then`, so a failed `LeaveAlternateScreen` would skip the mouse/paste
    // disables. Bind each separately so every restore step is truly attempted.
    let alt = execute!(stdout, LeaveAlternateScreen);
    let mouse = execute!(stdout, DisableMouseCapture);
    let paste = execute!(stdout, DisableBracketedPaste);
    let raw = disable_raw_mode();
    // Every step above ran; `and` just selects the first error to surface.
    pop.and(pointer).and(alt).and(mouse).and(paste).and(raw)?;
    Ok(())
}

/// Push the keyboard-enhancement flag (`DISAMBIGUATE_ESCAPE_CODES`) when
/// `supported`, recording it in `flag` only after the escape is written — so a
/// failed push never leaves [`restore`] popping a stack entry that was never
/// pushed. Factored over a generic writer so the push/record contract is
/// unit-testable without a real terminal.
fn push_kbd_enhancement(w: &mut impl Write, supported: bool, flag: &AtomicBool) -> io::Result<()> {
    if supported {
        queue!(
            w,
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        )?;
        w.flush()?;
        flag.store(true, Ordering::SeqCst);
    }
    Ok(())
}

/// Pop the keyboard-enhancement flag iff we pushed it. `swap(false)` makes this
/// idempotent across `restore`'s three callers (normal exit, panic hook,
/// setup-failure) — at most one pop, and none if we never pushed.
fn pop_kbd_enhancement(w: &mut impl Write, flag: &AtomicBool) -> io::Result<()> {
    if flag.swap(false, Ordering::SeqCst) {
        queue!(w, PopKeyboardEnhancementFlags)?;
        w.flush()?;
    }
    Ok(())
}

fn event_loop(terminal: &mut Tui, app: &mut App, watch_rx: Option<Receiver<()>>) -> Result<()> {
    terminal.draw(|frame| ui::draw(frame, app))?;
    let mut grab_pointer = false;
    while !app.should_quit {
        let mut redraw = false;
        // With a watcher active, wake periodically so the change channel gets
        // polled; without one, block until the next input event (no idle
        // wakeups). Then drain any already-queued input so a burst (e.g. a
        // wheel spin) collapses into one redraw, redrawing only when an event
        // changed something visible — motion events flood in but mostly don't.
        let has_input = match &watch_rx {
            Some(_) => event::poll(POLL_TIMEOUT)?,
            None => true,
        };
        if has_input {
            redraw |= handle_event(app, event::read()?);
            while !app.should_quit && event::poll(Duration::ZERO)? {
                redraw |= handle_event(app, event::read()?);
            }
        }
        // A settled filesystem change refreshes status + the open diff once,
        // draining any extra queued signals into the same reload.
        if let Some(rx) = &watch_rx {
            let mut changed = false;
            while rx.try_recv().is_ok() {
                changed = true;
            }
            if changed {
                app.reload();
                redraw = true;
            }
        }
        if app.should_quit {
            break;
        }
        if redraw {
            terminal.draw(|frame| ui::draw(frame, app))?;
        }
        // Mirror the hover/drag state in the OS pointer where the terminal
        // supports it, emitting the escape only on change.
        if app.divider_engaged() != grab_pointer {
            grab_pointer = !grab_pointer;
            set_pointer(grab_pointer)?;
        }
    }
    Ok(())
}

/// Handle one input event; returns whether the frame needs redrawing.
fn handle_event(app: &mut App, event: Event) -> bool {
    match event {
        Event::Key(key) if key.kind == KeyEventKind::Press => {
            app.on_key(key);
            true
        }
        // The event loop stamps the double-click clock as it dispatches (plan §3.6).
        Event::Mouse(mouse) => app.on_mouse_at(mouse, Instant::now()),
        // A bracketed paste: its newlines insert into the open comment editor
        // instead of each Enter saving the note (plan §3.5).
        Event::Paste(text) => {
            app.on_paste(&text);
            true
        }
        // A resize breaks a pending double-click chain before the redraw rebuilds
        // the layout (plan §3.6).
        Event::Resize(_, _) => {
            app.on_resize();
            true
        }
        _ => false,
    }
}

/// Request the split-bar grab pointer (or reset to the default) via OSC 22.
fn set_pointer(grab: bool) -> Result<()> {
    let seq = if grab { POINTER_GRAB } else { POINTER_DEFAULT };
    let mut stdout = io::stdout();
    stdout.write_all(seq.as_bytes())?;
    stdout.flush()?;
    Ok(())
}

/// Restore the terminal before the default panic handler prints, so a panic
/// never leaves the user in raw mode on the alternate screen.
fn install_panic_hook() {
    let original = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = restore();
        original(info);
    }));
}

/// Render a single frame to a string using the in-memory test backend. Used by
/// `--dump-frame` and by integration tests to assert on rendered output without
/// driving a real terminal.
pub fn dump_frame(app: &App, width: u16, height: u16) -> Result<String> {
    Ok(buffer_to_string(&render_to_buffer(app, width, height)?))
}

/// Render a single frame to the in-memory test backend and return the raw
/// [`Buffer`], so tests can assert on per-cell styling (e.g. foreground colours)
/// that `dump_frame`'s text serialization drops.
pub fn render_to_buffer(app: &App, width: u16, height: u16) -> Result<Buffer> {
    let mut terminal = Terminal::new(TestBackend::new(width, height))?;
    terminal.draw(|frame| ui::draw(frame, app))?;
    Ok(terminal.backend().buffer().clone())
}

fn buffer_to_string(buffer: &Buffer) -> String {
    let area = buffer.area;
    let mut out = String::with_capacity((area.width as usize + 1) * area.height as usize);
    for y in 0..area.height {
        for x in 0..area.width {
            out.push_str(buffer[(x, y)].symbol());
        }
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // Local `AtomicBool`s throughout — never the global `KBD_ENHANCED` — so the
    // push/pop state machine is exercised without cross-test interference.

    #[test]
    fn push_writes_and_records_when_supported() {
        let flag = AtomicBool::new(false);
        let mut sink: Vec<u8> = Vec::new();
        push_kbd_enhancement(&mut sink, true, &flag).unwrap();
        assert!(!sink.is_empty(), "a supported push writes the escape bytes");
        assert!(
            flag.load(Ordering::SeqCst),
            "flag set after a successful push"
        );
    }

    #[test]
    fn push_is_noop_when_unsupported() {
        let flag = AtomicBool::new(false);
        let mut sink: Vec<u8> = Vec::new();
        push_kbd_enhancement(&mut sink, false, &flag).unwrap();
        assert!(sink.is_empty(), "an unsupported push writes nothing");
        assert!(
            !flag.load(Ordering::SeqCst),
            "flag stays false when unsupported"
        );
    }

    #[test]
    fn pop_writes_and_clears_after_push() {
        let flag = AtomicBool::new(false);
        let mut sink: Vec<u8> = Vec::new();
        push_kbd_enhancement(&mut sink, true, &flag).unwrap();
        sink.clear();
        pop_kbd_enhancement(&mut sink, &flag).unwrap();
        assert!(
            !sink.is_empty(),
            "a pop after a push writes the escape bytes"
        );
        assert!(!flag.load(Ordering::SeqCst), "flag cleared after the pop");
    }

    #[test]
    fn pop_does_not_double_pop() {
        let flag = AtomicBool::new(false);
        let mut sink: Vec<u8> = Vec::new();
        push_kbd_enhancement(&mut sink, true, &flag).unwrap();
        pop_kbd_enhancement(&mut sink, &flag).unwrap();
        sink.clear();
        // Flag already false → a second pop must write nothing (no double-pop).
        pop_kbd_enhancement(&mut sink, &flag).unwrap();
        assert!(sink.is_empty(), "a second pop is a no-op");
        assert!(!flag.load(Ordering::SeqCst));
    }
}
