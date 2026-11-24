mod app;
mod external_diff;
mod input;
mod ui;

use app::{App, Mode};
use clap::Parser;
use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use external_diff::launch_external_diff;
use input::{handle_key, handle_mouse};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use btrw_core::config::Config;
use btrw_core::i18n::{Catalog, Locale};
use btrw_core::temp_snapshot::{
    cleanup_all_active_temp_snapshots, cleanup_orphaned_on_startup,
};
use std::io;
use std::path::PathBuf;
use std::time::Duration;

#[derive(Debug, Parser)]
#[command(
    name = "btrw",
    about = "Btrfs Rewind — Midnight Commander–style dual-pane Btrfs snapshot & time-travel TUI",
    version
)]
struct Cli {
    /// Override UI language (en, cs, auto)
    #[arg(long)]
    lang: Option<String>,

    /// Path to configuration file
    #[arg(long)]
    config: Option<PathBuf>,

    /// Base directory containing Btrfs snapshots
    #[arg(long)]
    snapshots_dir: Option<PathBuf>,

    /// Optional custom left directory (also for non-Btrfs testing)
    #[arg(long)]
    left: Option<PathBuf>,

    /// Optional custom right directory (also for non-Btrfs testing)
    #[arg(long)]
    right: Option<PathBuf>,

    /// Print resolved configuration as TOML and exit
    #[arg(long)]
    print_config: bool,
}

static TERMINATE_REQUESTED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

extern "C" fn handle_term_signal(_sig: libc::c_int) {
    TERMINATE_REQUESTED.store(true, std::sync::atomic::Ordering::Relaxed);
}

fn install_termination_signals() {
    unsafe {
        let mut sa: libc::sigaction = std::mem::zeroed();
        sa.sa_sigaction = handle_term_signal as *const () as usize;
        libc::sigemptyset(&mut sa.sa_mask);
        sa.sa_flags = 0;
        libc::sigaction(libc::SIGTERM, &sa, std::ptr::null_mut());
        libc::sigaction(libc::SIGHUP, &sa, std::ptr::null_mut());
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    if cli.left.is_some() != cli.right.is_some() {
        return Err("--left and --right must be used together".into());
    }

    let config_path = cli.config.unwrap_or_else(Config::default_path);
    let config = Config::load(&config_path)?;
    let locale = Locale::resolve(cli.lang.as_deref(), Some(&config.general.language));
    let catalog = Catalog::new(locale);

    if cli.print_config {
        println!("{}", config.to_toml_pretty()?);
        return Ok(());
    }

    install_termination_signals();

    // Only restore terminal & drain global registry if the main TUI thread panics;
    // a background worker panic unwinds its own RAII TempRoSnapshot without disrupting TUI.
    let main_thread = std::thread::current().id();
    let prev_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if std::thread::current().id() == main_thread {
            cleanup_all_active_temp_snapshots();
            let _ = disable_raw_mode();
        }
        prev_hook(info);
    }));

    let mut app = App::new(
        config,
        config_path,
        cli.lang,
        cli.snapshots_dir,
        catalog,
        cli.left,
        cli.right,
    );
    let _ = cleanup_orphaned_on_startup(app.effective_snapshots_dir());
    let run_res = run_tui(&mut app);
    cleanup_all_active_temp_snapshots();
    run_res?;
    Ok(())
}

fn run_tui(app: &mut App) -> io::Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let mut mouse_active = app.config.panels.mouse_enabled;
    if mouse_active {
        execute!(stdout, EnableMouseCapture)?;
    }
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = event_loop(&mut terminal, app, &mut mouse_active);

    if mouse_active {
        let _ = execute!(terminal.backend_mut(), DisableMouseCapture);
    }
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    result
}

fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
    mouse_active: &mut bool,
) -> io::Result<()> {
    while !app.should_quit {
        if TERMINATE_REQUESTED.load(std::sync::atomic::Ordering::Relaxed) {
            app.cancel_all_jobs();
            app.should_quit = true;
            break;
        }
        app.poll_background_jobs();
        if app.config.panels.mouse_enabled != *mouse_active {
            *mouse_active = app.config.panels.mouse_enabled;
            if *mouse_active {
                execute!(terminal.backend_mut(), EnableMouseCapture)?;
            } else {
                execute!(terminal.backend_mut(), DisableMouseCapture)?;
            }
        }
        terminal.draw(|frame| ui::draw(frame, app))?;

        if event::poll(Duration::from_millis(80))? {
            match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    app.notice = None;
                    if matches!(app.mode, Mode::Browse) && key.code == KeyCode::F(4) {
                        launch_external_diff(terminal, app, *mouse_active)?;
                    } else {
                        handle_key(app, key, terminal.size()?.height as usize);
                    }
                }
                Event::Mouse(mouse) if *mouse_active => {
                    handle_mouse(app, mouse);
                }
                _ => {}
            }
        }
    }
    app.cancel_all_jobs();
    Ok(())
}

