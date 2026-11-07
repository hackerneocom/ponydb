//! The binary owns subscriber installation; dependency targets stay disabled.
use tracing::{level_filters::LevelFilter, Subscriber};
use tracing_subscriber::{filter::filter_fn, fmt::MakeWriter, layer::SubscriberExt};

pub fn subscriber<W>(
    level: LevelFilter,
    format: &str,
    writer: W,
) -> Box<dyn Subscriber + Send + Sync>
where
    W: for<'a> MakeWriter<'a> + Send + Sync + 'static,
{
    let filter = filter_fn(move |meta| {
        (meta.target() == "ponydb" || meta.target().starts_with("ponydb::"))
            && *meta.level() <= level
    });
    if format == "json" {
        Box::new(
            tracing_subscriber::registry().with(filter).with(
                tracing_subscriber::fmt::layer()
                    .json()
                    .with_current_span(true)
                    .with_span_list(false)
                    .with_ansi(false)
                    .with_writer(writer),
            ),
        )
    } else {
        Box::new(
            tracing_subscriber::registry().with(filter).with(
                tracing_subscriber::fmt::layer()
                    .with_ansi(false)
                    .with_writer(writer),
            ),
        )
    }
}

pub fn init(config: &runtime_config::Logging) -> Result<(), &'static str> {
    let level = config.level.parse().map_err(|_| "Invalid logging level")?;
    tracing::subscriber::set_global_default(subscriber(level, &config.format, std::io::stderr))
        .map_err(|_| "Cannot initialize logging")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{self, Write},
        sync::{Arc, Mutex},
    };
    #[derive(Clone, Default)]
    struct Buffer(Arc<Mutex<Vec<u8>>>);
    impl Write for Buffer {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    impl<'a> MakeWriter<'a> for Buffer {
        type Writer = Self;
        fn make_writer(&'a self) -> Self {
            self.clone()
        }
    }
    #[test]
    fn formats_filter_levels_dependency_targets_and_escape_keys() {
        for format in ["json", "text"] {
            for level in [LevelFilter::WARN, LevelFilter::DEBUG, LevelFilter::TRACE] {
                let buffer = Buffer::default();
                let subscriber = subscriber(level, format, buffer.clone());
                tracing::subscriber::with_default(subscriber, || {
                    tracing::warn!(target: "aws_sdk_s3", body = "SECRET_DEPENDENCY_BODY");
                    tracing::warn!(target: "ponydb::test", event = "warning");
                    tracing::debug!(target: "ponydb::test", event = "debug_marker");
                    observability::cache("memory", "hit", Some("actual/key\nforged\u{1b}"));
                });
                let output = String::from_utf8(buffer.0.lock().unwrap().clone()).unwrap();
                assert!(!output.contains("SECRET_DEPENDENCY_BODY"));
                assert_eq!(output.contains("debug_marker"), level >= LevelFilter::DEBUG);
                assert_eq!(output.contains("cache_access"), level >= LevelFilter::TRACE);
                assert_eq!(
                    output.lines().count(),
                    if level == LevelFilter::WARN {
                        1
                    } else if level == LevelFilter::DEBUG {
                        2
                    } else {
                        3
                    }
                );
                if format == "json" {
                    for line in output.lines() {
                        let value: serde_json::Value = serde_json::from_str(line).unwrap();
                        if value["fields"]["event"] == "cache_access" {
                            assert_eq!(value["fields"]["key"], "actual/key\nforged\u{1b}");
                        }
                    }
                }
                if level == LevelFilter::TRACE {
                    assert!(output.contains("actual/key"));
                }
                assert!(!output.contains('\u{1b}'));
            }
        }
    }
}
