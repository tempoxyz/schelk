use std::time::{Duration, Instant};

use tracing::{Span, info, info_span, warn};

/// Emits structured start/completion timing events for a command step.
#[must_use]
pub struct StepTimer {
    operation: &'static str,
    step: &'static str,
    span: Span,
    start: Instant,
    finished: bool,
}

impl StepTimer {
    pub fn start(operation: &'static str, step: &'static str) -> Self {
        let span = info_span!("schelk_step", operation = operation, step = step);
        span.in_scope(|| info!(operation = operation, step = step, "started"));

        Self {
            operation,
            step,
            span,
            start: Instant::now(),
            finished: false,
        }
    }

    pub fn finish(self) -> Duration {
        self.finish_with(|operation, step, elapsed| {
            info!(
                operation = operation,
                step = step,
                elapsed_ms = elapsed_ms(elapsed),
                elapsed = %format_duration(elapsed),
                "completed"
            );
        })
    }

    pub fn finish_with(
        mut self,
        emit: impl FnOnce(&'static str, &'static str, Duration),
    ) -> Duration {
        self.finished = true;
        let elapsed = self.start.elapsed();
        self.span
            .in_scope(|| emit(self.operation, self.step, elapsed));
        elapsed
    }
}

impl Drop for StepTimer {
    fn drop(&mut self) {
        if !self.finished {
            let elapsed = self.start.elapsed();
            self.span.in_scope(|| {
                warn!(
                    operation = self.operation,
                    step = self.step,
                    elapsed_ms = elapsed_ms(elapsed),
                    elapsed = %format_duration(elapsed),
                    "step did not complete"
                );
            });
        }
    }
}

pub fn elapsed_ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

pub fn format_duration(duration: Duration) -> String {
    let secs = duration.as_secs_f64();
    if secs >= 60.0 {
        let mins = (secs / 60.0).floor();
        let remaining_secs = secs - (mins * 60.0);
        format!("{:.0}m {:.2}s", mins, remaining_secs)
    } else if secs >= 1.0 {
        format!("{:.2}s", secs)
    } else {
        format!("{:.0}ms", secs * 1000.0)
    }
}
