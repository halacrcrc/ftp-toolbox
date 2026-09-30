//! Flatten a `tracing` event into the pieces a UI log view can render.
//!
//! Sits in ftp-core next to [`crate::progress`] for the same reason that module
//! does: it is frontend-facing plumbing, and keeping it here makes it testable
//! without standing up a Tauri app — which on Windows means linking WebView2
//! just to assert on a string.

use tracing::Event;

/// Split an event into `(message, structured fields)` as `[name, value]` pairs.
///
/// `tracing` keeps the formatted message in a field literally named
/// `"message"`; every other field is structured context. Dropping those used to
/// leave the UI log view with a bare "tftp send complete" while the README
/// promises byte, block and elapsed counts — the numbers only ever reached the
/// terminal through the `fmt` layer.
///
/// Both `record_debug` and `record_str` are needed: a `%`-formatted (Display)
/// field arrives as `record_debug`, and `DisplayValue`'s `Debug` forwards to the
/// inner `Display`, so it renders *without* quotes. A `?`-formatted (Debug)
/// field keeps its quotes, which is the intended difference between the two.
pub fn event_parts(event: &Event<'_>) -> (String, Vec<(String, String)>) {
    struct FieldVisitor {
        message: String,
        fields: Vec<(String, String)>,
    }
    impl tracing::field::Visit for FieldVisitor {
        fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
            let rendered = format!("{value:?}");
            if field.name() == "message" {
                self.message = rendered;
            } else {
                self.fields.push((field.name().to_string(), rendered));
            }
        }
        fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
            if field.name() == "message" {
                self.message = value.to_string();
            } else {
                self.fields.push((field.name().to_string(), value.to_string()));
            }
        }
    }
    let mut visitor = FieldVisitor {
        message: String::new(),
        fields: Vec::new(),
    };
    event.record(&mut visitor);
    (visitor.message, visitor.fields)
}

#[cfg(test)]
mod tests {
    use super::event_parts;
    use std::sync::{Arc, Mutex};
    // `SubscriberExt` 提供 `Registry::with(..)`；prelude 是官方推荐的一把抓写法。
    use tracing_subscriber::prelude::*;
    use tracing_subscriber::Layer;

    type Captured = (String, Vec<(String, String)>);

    #[derive(Clone, Default)]
    struct Sink(Arc<Mutex<Vec<Captured>>>);

    impl<S: tracing::Subscriber> Layer<S> for Sink {
        fn on_event(
            &self,
            event: &tracing::Event<'_>,
            _ctx: tracing_subscriber::layer::Context<'_, S>,
        ) {
            self.0.lock().unwrap().push(event_parts(event));
        }
    }

    /// Run `f` with a subscriber that captures exactly one event.
    fn capture_one(f: impl FnOnce()) -> Captured {
        let sink = Sink::default();
        let collected = sink.0.clone();
        let dispatch = tracing::Dispatch::from(tracing_subscriber::registry().with(sink));
        tracing::dispatcher::with_default(&dispatch, f);
        let mut events = collected.lock().unwrap().clone();
        assert_eq!(events.len(), 1, "expected exactly one captured event");
        events.pop().unwrap()
    }

    fn field<'a>(fields: &'a [(String, String)], name: &str) -> &'a str {
        fields
            .iter()
            .find(|(k, _)| k == name)
            .unwrap_or_else(|| panic!("字段 {name} 被丢掉了，实际只有 {fields:?}"))
            .1
        .as_str()
    }

    #[test]
    fn plain_message_event_carries_no_fields() {
        let (message, fields) = capture_one(|| tracing::info!("server started"));
        assert_eq!(message, "server started");
        assert!(fields.is_empty());
    }

    #[test]
    fn structured_fields_survive_the_trip_to_the_ui() {
        // 这正是 TFTP 传输完成时打的那条事件：README 承诺在界面日志里能看见
        // bytes / blocks / elapsed_ms，以前它们只出现在终端。
        let (message, fields) = capture_one(|| {
            tracing::info!(bytes = 8192u64, blocks = 16u32, elapsed_ms = 42u64, "tftp send complete");
        });
        assert_eq!(message, "tftp send complete");
        assert_eq!(fields.len(), 3, "三个结构化字段一个都不能丢：{fields:?}");
        assert_eq!(field(&fields, "bytes"), "8192");
        assert_eq!(field(&fields, "blocks"), "16");
        assert_eq!(field(&fields, "elapsed_ms"), "42");
    }

    #[test]
    fn display_formatted_values_render_without_quotes() {
        let path = String::from("/srv/pub/a.txt");
        let (message, fields) = capture_one(|| tracing::info!(path = %path, "wrote file"));
        assert_eq!(message, "wrote file");
        // `%` 走 Display，DisplayValue 的 Debug 转发给内部 Display，所以没有引号。
        assert_eq!(field(&fields, "path"), "/srv/pub/a.txt");
    }

    #[test]
    fn debug_formatted_values_keep_their_quotes() {
        let path = String::from("/srv/pub/a.txt");
        let (_, fields) = capture_one(|| tracing::info!(path = ?path, "wrote file"));
        // 与上一条成对照：`?` 走 Debug，字符串会被加上引号——这是预期差异。
        assert_eq!(field(&fields, "path"), "\"/srv/pub/a.txt\"");
    }

    #[test]
    fn message_is_never_mistaken_for_a_structured_field() {
        let (_, fields) = capture_one(|| tracing::info!(total = 7u8, "done"));
        assert!(
            fields.iter().all(|(k, _)| k != "message"),
            "message 不应出现在结构化字段里：{fields:?}"
        );
        assert_eq!(fields.len(), 1);
    }
}
