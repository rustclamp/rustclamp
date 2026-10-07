//! Counters and gauges with a Prometheus text renderer.
//!
//! Enabled by the `metrics` feature. Std only. With the `web` feature,
//! `Router::metrics` serves a registry at a route and counts requests into
//! it; elsewhere, answer a scrape with [`Registry::render`].
//! An HTTP (Axum) app uses `rustclamp_http::with_metrics` and passes
//! `render` as its extra metrics.
//!
//! ```
//! use rustclamp::metrics::Registry;
//!
//! let metrics = Registry::new();
//! metrics.inc("http_requests_total");
//! metrics.add("http_requests_total", 2);
//! metrics.set("queue_depth", -1);
//! let text = metrics.render();
//! assert!(text.contains("http_requests_total 3\n"));
//! assert!(text.contains("queue_depth -1\n"));
//! ```

use std::collections::BTreeMap;
use std::fmt::Write;
use std::sync::{Mutex, MutexGuard};

enum Metric {
    Counter(u64),
    Gauge(i64),
}

/// A set of named counters (only go up) and gauges (any value).
///
/// A name is created on first use; using one name as both kinds panics.
#[derive(Default)]
pub struct Registry {
    metrics: Mutex<BTreeMap<String, Metric>>,
}

impl Registry {
    /// An empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds one to counter `name`.
    pub fn inc(&self, name: &str) {
        self.add(name, 1);
    }

    /// Adds `by` to counter `name`.
    pub fn add(&self, name: &str, by: u64) {
        self.update(name, Metric::Counter(0), |metric| match metric {
            Metric::Counter(value) => *value = value.saturating_add(by),
            Metric::Gauge(_) => panic!("metric {name} is a gauge"),
        });
    }

    /// Sets gauge `name` to `value`.
    pub fn set(&self, name: &str, value: i64) {
        self.update(name, Metric::Gauge(0), |metric| match metric {
            Metric::Gauge(current) => *current = value,
            Metric::Counter(_) => panic!("metric {name} is a counter"),
        });
    }

    /// Every metric in the Prometheus text exposition format, sorted by name.
    pub fn render(&self) -> String {
        let mut out = String::new();
        for (name, metric) in self.lock().iter() {
            let _ = match metric {
                Metric::Counter(value) => writeln!(out, "# TYPE {name} counter\n{name} {value}"),
                Metric::Gauge(value) => writeln!(out, "# TYPE {name} gauge\n{name} {value}"),
            };
        }
        out
    }

    fn update(&self, name: &str, new: Metric, apply: impl FnOnce(&mut Metric)) {
        assert!(valid(name), "invalid metric name {name:?}");
        let mut metrics = self.lock();
        if let Some(metric) = metrics.get_mut(name) {
            return apply(metric); // no allocation once the name exists
        }
        apply(metrics.entry(name.to_string()).or_insert(new));
    }

    fn lock(&self) -> MutexGuard<'_, BTreeMap<String, Metric>> {
        self.metrics
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// `[a-zA-Z_:][a-zA-Z0-9_:]*`, the Prometheus metric name grammar.
fn valid(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_' || c == ':')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == ':')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_sorted_counters_and_gauges() {
        let metrics = Registry::new();
        metrics.set("b_gauge", 5);
        metrics.inc("a_total");
        metrics.add("a_total", 4);
        metrics.set("b_gauge", -2);
        assert_eq!(
            metrics.render(),
            "# TYPE a_total counter\na_total 5\n# TYPE b_gauge gauge\nb_gauge -2\n"
        );
    }

    #[test]
    #[should_panic(expected = "invalid metric name")]
    fn rejects_bad_names() {
        Registry::new().inc("bad name");
    }

    #[test]
    #[should_panic(expected = "is a counter")]
    fn rejects_kind_change() {
        let metrics = Registry::new();
        metrics.inc("x");
        metrics.set("x", 1);
    }
}
