//! Human-readable restore progress goes exclusively to stderr.
use database_service::restore::{Progress, RestoreProgress, RestoreStage};
use std::{
    io::{IsTerminal, Write},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

struct Renderer<W: Write> {
    writer: W,
    terminal: bool,
    started: Instant,
    last_render: Option<Instant>,
    last_stage: Option<RestoreStage>,
    current: Option<RestoreProgress>,
}
impl<W: Write> Renderer<W> {
    fn render(&mut self, progress: RestoreProgress, now: Instant) {
        self.current = Some(progress);
        let interval = if self.terminal {
            Duration::from_millis(200)
        } else {
            Duration::from_secs(5)
        };
        if self.last_stage == Some(progress.stage)
            && (progress.stage == RestoreStage::Complete
                || self
                    .last_render
                    .is_some_and(|last| now.duration_since(last) < interval))
        {
            return;
        }
        let text = format_progress(progress, now.duration_since(self.started));
        if self.terminal {
            let _ = write!(self.writer, "\r\x1b[2K{text}");
        } else {
            let _ = writeln!(self.writer, "{text}");
        }
        let _ = self.writer.flush();
        self.last_render = Some(now);
        self.last_stage = Some(progress.stage);
    }
    fn finish(&mut self) {
        if self.terminal && self.last_render.is_some() {
            let _ = writeln!(self.writer);
            let _ = self.writer.flush();
        }
    }
}
fn format_progress(progress: RestoreProgress, elapsed: Duration) -> String {
    let label = match progress.stage {
        RestoreStage::Copying => "copying backup",
        RestoreStage::Validating => "validating SQLite",
        RestoreStage::Hashing => "computing SHA-256",
        RestoreStage::Uploading => "uploading data segments",
        RestoreStage::Building => "building index and snapshot",
        RestoreStage::Publishing => "publishing database HEAD",
        RestoreStage::Complete => "complete",
    };
    let mut text = format!("Restore: {label}");
    if matches!(
        progress.stage,
        RestoreStage::Copying | RestoreStage::Hashing | RestoreStage::Uploading
    ) {
        let percent = if progress.total_bytes == 0 {
            0.0
        } else {
            progress.completed_bytes as f64 / progress.total_bytes as f64 * 100.0
        };
        text.push_str(&format!(
            " — {}/{} bytes ({percent:.1}%)",
            progress.completed_bytes, progress.total_bytes
        ));
    }
    text.push_str(&format!(" — {:.1}s elapsed", elapsed.as_secs_f64()));
    text
}

pub struct ProgressDisplay {
    renderer: Arc<Mutex<Renderer<std::io::Stderr>>>,
    heartbeat: tokio::task::JoinHandle<()>,
}
impl ProgressDisplay {
    pub fn start() -> (Self, Progress) {
        let renderer = Arc::new(Mutex::new(Renderer {
            writer: std::io::stderr(),
            terminal: std::io::stderr().is_terminal(),
            started: Instant::now(),
            last_render: None,
            last_stage: None,
            current: None,
        }));
        let updates = renderer.clone();
        let callback: Progress = Arc::new(move |progress| {
            updates.lock().unwrap().render(progress, Instant::now());
        });
        let ticks = renderer.clone();
        let heartbeat = tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_millis(200));
            loop {
                interval.tick().await;
                let mut renderer = ticks.lock().unwrap();
                if let Some(progress) = renderer.current {
                    renderer.render(progress, Instant::now());
                }
            }
        });
        (
            Self {
                renderer,
                heartbeat,
            },
            callback,
        )
    }
}
impl Drop for ProgressDisplay {
    fn drop(&mut self) {
        self.heartbeat.abort();
        self.renderer.lock().unwrap().finish();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn redirected_progress_is_throttled_but_stages_are_immediate() {
        let start = Instant::now();
        let mut renderer = Renderer {
            writer: Vec::new(),
            terminal: false,
            started: start,
            last_render: None,
            last_stage: None,
            current: None,
        };
        let mut progress = RestoreProgress {
            stage: RestoreStage::Uploading,
            completed_bytes: 0,
            total_bytes: 100,
        };
        renderer.render(progress, start);
        progress.completed_bytes = 50;
        renderer.render(progress, start + Duration::from_secs(1));
        renderer.render(progress, start + Duration::from_secs(5));
        progress.stage = RestoreStage::Publishing;
        renderer.render(progress, start + Duration::from_secs(5));
        let output = String::from_utf8(renderer.writer).unwrap();
        assert_eq!(output.lines().count(), 3);
        assert!(output.contains("50/100 bytes (50.0%)"));
        assert!(!output.lines().last().unwrap().contains('%'));
        assert!(!output.contains('\r'));
    }
    #[test]
    fn terminal_progress_refreshes_and_ends_before_errors_or_results() {
        let start = Instant::now();
        let mut renderer = Renderer {
            writer: Vec::new(),
            terminal: true,
            started: start,
            last_render: None,
            last_stage: None,
            current: None,
        };
        let progress = RestoreProgress {
            stage: RestoreStage::Validating,
            completed_bytes: 0,
            total_bytes: 100,
        };
        renderer.render(progress, start);
        renderer.render(progress, start + Duration::from_millis(100));
        renderer.render(progress, start + Duration::from_millis(200));
        renderer.finish();
        let output = String::from_utf8(renderer.writer).unwrap();
        assert_eq!(output.matches("validating SQLite").count(), 2);
        assert!(output.ends_with('\n'));
        assert!(!output.contains('%'));
    }
}
