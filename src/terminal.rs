use std::io::{self, Stdout, Write};
use std::sync::mpsc::Receiver;
use std::time::Duration;

use anyhow::Result;
use ratatui::backend::{CrosstermBackend, TestBackend};
use ratatui::buffer::Buffer;
use ratatui::crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyEventKind,
};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
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
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    write!(stdout, "{ENABLE_MOUSE_MOTION}")?;
    stdout.flush()?;
    Ok(Terminal::new(CrosstermBackend::new(stdout))?)
}

fn restore() -> Result<()> {
    let mut stdout = io::stdout();
    // Undo the motion request and any custom pointer shape before leaving, so
    // the terminal isn't left reporting movement or showing a grab cursor.
    write!(stdout, "{POINTER_DEFAULT}{DISABLE_MOUSE_MOTION}")?;
    execute!(stdout, LeaveAlternateScreen, DisableMouseCapture)?;
    disable_raw_mode()?;
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
        Event::Mouse(mouse) => app.on_mouse(mouse),
        Event::Resize(_, _) => true,
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
    let mut terminal = Terminal::new(TestBackend::new(width, height))?;
    terminal.draw(|frame| ui::draw(frame, app))?;
    Ok(buffer_to_string(terminal.backend().buffer()))
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
