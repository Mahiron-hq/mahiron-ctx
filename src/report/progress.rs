use std::io::{IsTerminal, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

/// Progress indication, addressed by the engine without knowing whether anything is
/// actually displayed.
pub trait Progress: Send + Sync {
    fn phase(&self, _label: &str) {}
    fn discovered(&self, _count: usize) {}
    /// Retire the current phase with a line that stays on screen.
    fn phase_complete(&self, _summary: &str) {}
    fn packaged(&self, _count: usize, _total: usize) {}
    /// Bytes of the assembled document delivered so far.
    fn written(&self, _done: u64, _total: u64) {}
    fn finish(&self) {}
}

/// Progress sink for non-interactive runs, scripted callers and quiet mode.
#[derive(Debug, Default, Clone, Copy)]
pub struct NullProgress;

impl Progress for NullProgress {}

/// Redraw interval. Fast enough that the animation reads as motion, slow enough that a
/// run producing thousands of events per second does not spend its time drawing.
const FRAME_INTERVAL: Duration = Duration::from_millis(80);

/// Deliberately ASCII. A legacy console on a non-UTF-8 code page renders anything else as
/// replacement characters, and the progress line is the first thing a user ever sees.
const SPINNER: [&str; 4] = ["|", "/", "-", "\\"];
const BAR_FILLED: &str = "#";
const BAR_EMPTY: &str = ".";
const BAR_WIDTH: usize = 24;

struct State {
    stream: Mutex<Box<dyn Write + Send>>,
    phase: Mutex<String>,
    discovered: AtomicUsize,
    packaged: AtomicUsize,
    total: AtomicUsize,
    /// Unit the proportional bar is measured in, once one applies.
    unit: Mutex<&'static str>,
    frame: AtomicU64,
    /// Width of the last line drawn, so the next one can cover it completely without
    /// relying on a terminal escape the host may not interpret.
    drawn_width: AtomicUsize,
    running: AtomicBool,
    /// Nothing is drawn until a phase begins. The indicator is constructed before the
    /// run starts, and the run may still need to ask the user a question first; a
    /// spinner animating over the prompt makes it unreadable and swallows the answer.
    armed: AtomicBool,
}

impl State {
    fn render(&self) {
        if !self.running.load(Ordering::Relaxed) || !self.armed.load(Ordering::Relaxed) {
            return;
        }
        let frame = self.frame.fetch_add(1, Ordering::Relaxed) as usize;
        let spinner = SPINNER[frame % SPINNER.len()];
        let total = self.total.load(Ordering::Relaxed);

        let line = if total > 0 {
            let packaged = self.packaged.load(Ordering::Relaxed).min(total);
            let filled = packaged * BAR_WIDTH / total;
            let percent = packaged * 100 / total;
            let unit = match self.unit.lock() {
                Ok(guard) => *guard,
                Err(poisoned) => *poisoned.into_inner(),
            };
            let phase = match self.phase.lock() {
                Ok(guard) => guard.clone(),
                Err(poisoned) => poisoned.into_inner().clone(),
            };
            format!(
                "{spinner} {phase}  [{}{}] {percent:>3}%  {packaged}/{total} {unit}",
                BAR_FILLED.repeat(filled),
                BAR_EMPTY.repeat(BAR_WIDTH - filled),
            )
        } else {
            let discovered = self.discovered.load(Ordering::Relaxed);
            let phase = match self.phase.lock() {
                Ok(guard) => guard.clone(),
                Err(poisoned) => poisoned.into_inner().clone(),
            };
            if discovered > 0 {
                format!("{spinner} {phase}  {discovered} files found")
            } else {
                format!("{spinner} {phase}")
            }
        };

        self.draw(&line);
    }

    fn draw(&self, line: &str) {
        let width = line.chars().count();
        let previous = self.drawn_width.swap(width, Ordering::Relaxed);
        let padding = previous.saturating_sub(width);

        let mut stream = match self.stream.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        let _ = write!(stream, "\r{line}{}", " ".repeat(padding));
        let _ = stream.flush();
    }

    fn set_unit(&self, unit: &'static str) {
        match self.unit.lock() {
            Ok(mut guard) => *guard = unit,
            Err(poisoned) => *poisoned.into_inner() = unit,
        }
    }

    fn clear(&self) {
        let width = self.drawn_width.swap(0, Ordering::Relaxed);
        let mut stream = match self.stream.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        let _ = write!(stream, "\r{}\r", " ".repeat(width));
        let _ = stream.flush();
    }
}

/// Animated indicator for an attached terminal.
///
/// A dedicated thread drives the redraw rather than the engine doing it inline: work that
/// produces no events for several seconds — resolving a large tree, retrieving a remote
/// source — would otherwise look like a hang.
pub struct TerminalProgress {
    state: Arc<State>,
    ticker: Option<JoinHandle<()>>,
}

impl TerminalProgress {
    /// Returns an indicator only when stderr is a terminal, so redirected runs stay clean.
    pub fn if_interactive() -> Option<Self> {
        if !std::io::stderr().is_terminal() {
            return None;
        }
        Some(Self::to_stream(Box::new(std::io::stderr())))
    }

    fn to_stream(stream: Box<dyn Write + Send>) -> Self {
        let state = Arc::new(State {
            stream: Mutex::new(stream),
            phase: Mutex::new("starting".to_string()),
            discovered: AtomicUsize::new(0),
            packaged: AtomicUsize::new(0),
            total: AtomicUsize::new(0),
            unit: Mutex::new("files"),
            frame: AtomicU64::new(0),
            drawn_width: AtomicUsize::new(0),
            running: AtomicBool::new(true),
            armed: AtomicBool::new(false),
        });

        let ticker = {
            let state = Arc::clone(&state);
            std::thread::spawn(move || {
                while state.running.load(Ordering::Relaxed) {
                    state.render();
                    std::thread::sleep(FRAME_INTERVAL);
                }
            })
        };

        Self {
            state,
            ticker: Some(ticker),
        }
    }

    fn stop(&mut self) {
        if !self.state.running.swap(false, Ordering::Relaxed) {
            return;
        }
        if let Some(ticker) = self.ticker.take() {
            let _ = ticker.join();
        }
        self.state.clear();
    }
}

impl Progress for TerminalProgress {
    fn phase(&self, label: &str) {
        match self.state.phase.lock() {
            Ok(mut guard) => *guard = label.to_string(),
            Err(poisoned) => *poisoned.into_inner() = label.to_string(),
        }
        self.state.armed.store(true, Ordering::Relaxed);
        self.state.render();
    }

    fn discovered(&self, count: usize) {
        // Storing only: the ticker owns the redraw, so a walk finding thousands of files
        // per second costs one atomic store each rather than one write to the terminal.
        self.state.discovered.store(count, Ordering::Relaxed);
    }

    fn phase_complete(&self, summary: &str) {
        self.state.armed.store(false, Ordering::Relaxed);
        self.state.discovered.store(0, Ordering::Relaxed);
        self.state.total.store(0, Ordering::Relaxed);
        self.state.packaged.store(0, Ordering::Relaxed);
        self.state.clear();
        let mut stream = match self.state.stream.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        let _ = writeln!(stream, "  {summary}");
        let _ = stream.flush();
    }

    fn packaged(&self, count: usize, total: usize) {
        self.state.armed.store(true, Ordering::Relaxed);
        self.state.set_unit("files");
        self.state.total.store(total, Ordering::Relaxed);
        self.state.packaged.store(count, Ordering::Relaxed);
    }

    fn written(&self, done: u64, total: u64) {
        self.state.armed.store(true, Ordering::Relaxed);
        self.state.set_unit("KB");
        self.state
            .total
            .store((total / 1024).max(1) as usize, Ordering::Relaxed);
        self.state
            .packaged
            .store((done / 1024) as usize, Ordering::Relaxed);
    }

    fn finish(&self) {
        self.state.running.store(false, Ordering::Relaxed);
        self.state.clear();
    }
}

impl Drop for TerminalProgress {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Default)]
    struct Captured(Arc<Mutex<Vec<u8>>>);

    impl Write for Captured {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn rendered(captured: &Captured) -> String {
        String::from_utf8(captured.0.lock().unwrap().clone()).unwrap()
    }

    #[test]
    fn nothing_is_drawn_before_a_phase_begins() {
        let captured = Captured::default();
        let progress = TerminalProgress::to_stream(Box::new(captured.clone()));
        std::thread::sleep(FRAME_INTERVAL * 3);
        let quiet = rendered(&captured);
        progress.finish();
        assert!(
            quiet.is_empty(),
            "drew before it had anything to say: {quiet:?}"
        );
    }

    #[test]
    fn discovery_animates_even_while_nothing_is_reported() {
        let captured = Captured::default();
        let progress = TerminalProgress::to_stream(Box::new(captured.clone()));
        progress.phase("discovering");
        std::thread::sleep(FRAME_INTERVAL * 4);
        progress.finish();

        let output = rendered(&captured);
        assert!(output.contains("discovering"));
        let frames = SPINNER.iter().filter(|f| output.contains(**f)).count();
        assert!(frames > 1, "the indicator never animated: {output:?}");
    }

    #[test]
    fn packaging_shows_proportion_once_a_total_is_known() {
        let captured = Captured::default();
        let progress = TerminalProgress::to_stream(Box::new(captured.clone()));
        progress.phase("packaging");
        progress.packaged(4, 4);
        std::thread::sleep(FRAME_INTERVAL * 2);
        progress.finish();

        let output = rendered(&captured);
        assert!(output.contains("100%"), "no proportion shown: {output:?}");
        assert!(output.contains("4/4 files"));
    }

    #[test]
    fn writing_the_document_reports_its_own_progress() {
        let captured = Captured::default();
        let progress = TerminalProgress::to_stream(Box::new(captured.clone()));
        progress.phase("writing");
        progress.written(512 * 1024, 1024 * 1024);
        std::thread::sleep(FRAME_INTERVAL * 2);
        progress.finish();

        let output = rendered(&captured);
        assert!(output.contains("KB"), "no byte progress shown: {output:?}");
        assert!(output.contains(" 50%"), "{output:?}");
    }

    #[test]
    fn a_finished_run_leaves_the_line_blank() {
        let captured = Captured::default();
        let progress = TerminalProgress::to_stream(Box::new(captured.clone()));
        progress.phase("discovering a very long phase name");
        progress.finish();
        assert!(rendered(&captured).ends_with('\r'));
    }
}
