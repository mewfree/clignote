pub mod app;
pub mod config;
pub mod fold;
pub mod git;
pub mod journal;
pub mod keymap;
mod links;
pub mod pane;
pub mod picker;
pub mod structure;
pub mod ui;
pub mod wrap;

pub use app::App;

use std::io;

use crossterm::{
    cursor::SetCursorStyle,
    event::{self, DisableMouseCapture, EnableMouseCapture, Event},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};

pub fn run(file_path: Option<&str>) -> anyhow::Result<()> {
    let mut terminal = setup_terminal()?;
    let mut app = App::new(file_path)?;

    loop {
        terminal.draw(|f| ui::render(f, &mut app))?;
        // Thin bar in Insert mode; otherwise the user's own cursor shape.
        let style = match app.mode {
            app::Mode::Insert => SetCursorStyle::SteadyBar,
            _ => SetCursorStyle::DefaultUserShape,
        };
        execute!(terminal.backend_mut(), style)?;

        // Block until something happens; there are no timers, so redrawing on
        // an idle poll would only burn CPU (and re-run the git diff). The one
        // exception is a running commit-and-push, which animates a spinner.
        if app.commit_job.is_some() {
            if !event::poll(std::time::Duration::from_millis(100))? {
                app.poll_commit_job();
                continue;
            }
            if app.poll_commit_job() {
                continue;
            }
        }
        match event::read()? {
            Event::Key(key) => app.handle_key(key),
            Event::Mouse(mouse) => app.handle_mouse(mouse),
            _ => {}
        }

        if app.should_quit {
            break;
        }
    }

    restore_terminal(terminal)?;
    Ok(())
}

fn setup_terminal() -> anyhow::Result<Terminal<CrosstermBackend<io::Stdout>>> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    Ok(Terminal::new(CrosstermBackend::new(stdout))?)
}

fn restore_terminal(mut terminal: Terminal<CrosstermBackend<io::Stdout>>) -> anyhow::Result<()> {
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        SetCursorStyle::DefaultUserShape,
        DisableMouseCapture,
        LeaveAlternateScreen
    )?;
    terminal.show_cursor()?;
    Ok(())
}
