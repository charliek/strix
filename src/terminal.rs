use std::io::{self, Stdout, Write};

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
/// kitty, WezTerm, Alacritty, Ghostty, …; silently ignored elsewhere. An empty
/// name resets to the terminal default.
const POINTER_COL_RESIZE: &str = "\x1b]22;col-resize\x1b\\";
const POINTER_DEFAULT: &str = "\x1b]22;\x1b\\";

/// Set up the terminal, run the event loop, and restore the terminal on the way
/// out (including on panic, via the installed hook).
pub fn run(mut app: App) -> Result<()> {
    install_panic_hook();
    let mut terminal = setup()?;
    let result = event_loop(&mut terminal, &mut app);
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
    // the terminal isn't left reporting movement or showing a resize cursor.
    write!(stdout, "{POINTER_DEFAULT}{DISABLE_MOUSE_MOTION}")?;
    execute!(stdout, LeaveAlternateScreen, DisableMouseCapture)?;
    disable_raw_mode()?;
    Ok(())
}

fn event_loop(terminal: &mut Tui, app: &mut App) -> Result<()> {
    terminal.draw(|frame| ui::draw(frame, app))?;
    let mut resize_pointer = false;
    while !app.should_quit {
        // Only redraw when an event changed something visible. Motion events
        // (from mouse tracking) flood in, so most produce no change.
        let redraw = match event::read()? {
            Event::Key(key) if key.kind == KeyEventKind::Press => {
                app.on_key(key);
                true
            }
            Event::Mouse(mouse) => app.on_mouse(mouse),
            Event::Resize(_, _) => true,
            _ => false,
        };
        if app.should_quit {
            break;
        }
        if redraw {
            terminal.draw(|frame| ui::draw(frame, app))?;
        }
        // Mirror the hover/drag state in the OS pointer where the terminal
        // supports it, emitting the escape only on change.
        if app.divider_engaged() != resize_pointer {
            resize_pointer = !resize_pointer;
            set_pointer(resize_pointer)?;
        }
    }
    Ok(())
}

/// Request the split-bar resize pointer (or reset to the default) via OSC 22.
fn set_pointer(col_resize: bool) -> Result<()> {
    let seq = if col_resize {
        POINTER_COL_RESIZE
    } else {
        POINTER_DEFAULT
    };
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
