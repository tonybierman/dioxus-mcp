//! Runtime telemetry probe for Dioxus apps.
//!
//! Installs a [`tracing`] subscriber that captures Dioxus runtime events
//! (renders, signal writes, server-fn calls) plus a panic hook, and writes
//! the stream as JSON-lines under `target/dioxus-mcp/events.jsonl`. The
//! dioxus-mcp server's `runtime_events` tool reads that log on demand.
//!
//! Quick start:
//!
//! ```no_run
//! fn main() {
//!     dioxus_mcp_probe::install();
//!     // your dioxus app entrypoint
//! }
//! ```
//!
//! In release builds the probe is a no-op unless the `force` cargo feature
//! is enabled, so it's safe to leave the `install()` call in `main`.

#![allow(clippy::needless_doctest_main)]

use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::Duration;

use serde::Serialize;
use serde_json::Value;
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id};
use tracing::{Event, Subscriber};
use tracing_subscriber::layer::{Context, Layer, SubscriberExt};
use tracing_subscriber::registry::{LookupSpan, Registry};

const DEFAULT_QUEUE_CAP: usize = 8192;
const DEFAULT_ROTATE_BYTES: u64 = 10 * 1024 * 1024;
const DEFAULT_ROTATIONS_KEPT: usize = 3;
const FLUSH_INTERVAL: Duration = Duration::from_millis(100);

/// Targets we instrument. Events from other targets are ignored.
const KNOWN_TARGETS: &[&str] = &[
    "dioxus",
    "dioxus_core",
    "dioxus_signals",
    "dioxus_router",
    "dioxus_fullstack",
    "dioxus_mcp_probe",
];

/// Tunables for [`install_with`]. All paths are resolved relative to the
/// process's current directory at `install` time.
#[derive(Debug, Clone)]
pub struct ProbeConfig {
    /// Where to write the live log. Default: `target/dioxus-mcp/events.jsonl`
    /// resolved against the CWD.
    pub log_path: PathBuf,
    /// Rotate the live file once it exceeds this many bytes. Default: 10 MiB.
    pub rotate_bytes: u64,
    /// Number of rotated files to keep on disk. Default: 3.
    pub rotations_kept: usize,
    /// Bounded queue capacity. Overflow is dropped (counted in
    /// [`ProbeHandle::dropped_events`]) rather than blocking the producer.
    pub queue_capacity: usize,
    /// Extra `tracing` targets to capture beyond the built-in Dioxus ones.
    pub extra_targets: Vec<String>,
}

impl Default for ProbeConfig {
    fn default() -> Self {
        Self {
            log_path: default_log_path(),
            rotate_bytes: DEFAULT_ROTATE_BYTES,
            rotations_kept: DEFAULT_ROTATIONS_KEPT,
            queue_capacity: DEFAULT_QUEUE_CAP,
            extra_targets: Vec::new(),
        }
    }
}

fn default_log_path() -> PathBuf {
    std::env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join("target")
        .join("dioxus-mcp")
        .join("events.jsonl")
}

/// Handle returned by [`install`]. Dropping the handle stops the writer
/// thread and flushes the buffer. Most apps install once at startup and
/// leak the handle for the process lifetime.
pub struct ProbeHandle {
    pub(crate) dropped: Arc<AtomicU64>,
    pub(crate) log_path: PathBuf,
    pub(crate) sender: Option<SyncSender<LogRecord>>,
    pub(crate) thread: Option<thread::JoinHandle<()>>,
    pub(crate) shutdown: Arc<AtomicBool>,
}

impl ProbeHandle {
    /// Number of events dropped so far due to a full queue.
    pub fn dropped_events(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    /// Path of the live JSONL file.
    pub fn log_path(&self) -> &Path {
        &self.log_path
    }
}

impl Drop for ProbeHandle {
    fn drop(&mut self) {
        // Signal shutdown before dropping our sender. The panic hook holds
        // a separate sender clone for the process lifetime, so we can't rely
        // on channel-disconnect to wake the writer.
        self.shutdown.store(true, Ordering::Relaxed);
        drop(self.sender.take());
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Install the probe with default config. In release builds (without the
/// `force` feature) this is a no-op and returns a dormant handle.
pub fn install() -> ProbeHandle {
    install_with(ProbeConfig::default())
}

/// Install the probe with a custom config. See [`install`] for the
/// release-build behavior.
pub fn install_with(config: ProbeConfig) -> ProbeHandle {
    if !is_active() {
        return ProbeHandle {
            dropped: Arc::new(AtomicU64::new(0)),
            log_path: config.log_path,
            sender: None,
            thread: None,
            shutdown: Arc::new(AtomicBool::new(false)),
        };
    }
    install_active(config)
}

#[cfg(any(debug_assertions, feature = "force"))]
fn is_active() -> bool {
    true
}

#[cfg(not(any(debug_assertions, feature = "force")))]
fn is_active() -> bool {
    false
}

fn install_active(config: ProbeConfig) -> ProbeHandle {
    if let Some(parent) = config.log_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    let (sender, receiver) = sync_channel::<LogRecord>(config.queue_capacity);
    let dropped = Arc::new(AtomicU64::new(0));
    let shutdown = Arc::new(AtomicBool::new(false));

    let writer_cfg = config.clone();
    let writer_shutdown = shutdown.clone();
    let thread = thread::Builder::new()
        .name("dioxus-mcp-probe-writer".into())
        .spawn(move || writer_loop(receiver, writer_cfg, writer_shutdown))
        .expect("spawn probe writer thread");

    install_panic_hook(sender.clone(), dropped.clone());

    let targets: Vec<String> = KNOWN_TARGETS
        .iter()
        .map(|s| (*s).to_string())
        .chain(config.extra_targets.iter().cloned())
        .collect();

    let layer = ProbeLayer {
        sender: sender.clone(),
        dropped: dropped.clone(),
        targets,
    };

    let subscriber = Registry::default().with(layer);
    // Best-effort: if a subscriber is already installed, log to stderr but
    // keep going — the panic hook and the writer thread are still useful.
    let _ = tracing::subscriber::set_global_default(subscriber);

    ProbeHandle {
        dropped,
        log_path: config.log_path,
        sender: Some(sender),
        thread: Some(thread),
        shutdown,
    }
}

// ---------- event schema ----------

#[derive(Debug, Serialize, Clone)]
struct LogRecord {
    v: u8,
    ts: String,
    kind: String,
    #[serde(flatten)]
    fields: serde_json::Map<String, Value>,
}

fn now_rfc3339() -> String {
    let now = time::OffsetDateTime::now_utc();
    now.format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| String::from("1970-01-01T00:00:00Z"))
}

fn make_record(kind: &str, fields: serde_json::Map<String, Value>) -> LogRecord {
    LogRecord {
        v: 1,
        ts: now_rfc3339(),
        kind: kind.to_string(),
        fields,
    }
}

fn enqueue(sender: &SyncSender<LogRecord>, dropped: &Arc<AtomicU64>, record: LogRecord) {
    match sender.try_send(record) {
        Ok(()) => {}
        Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => {
            dropped.fetch_add(1, Ordering::Relaxed);
        }
    }
}

// ---------- writer thread ----------

fn writer_loop(receiver: Receiver<LogRecord>, config: ProbeConfig, shutdown: Arc<AtomicBool>) {
    let mut writer = open_writer(&config.log_path);
    let mut bytes_in_file = current_size(&config.log_path);
    let mut last_flush = std::time::Instant::now();

    let write_record =
        |writer: &mut Option<BufWriter<File>>, bytes_in_file: &mut u64, record: LogRecord| {
            let Ok(line) = serde_json::to_string(&record) else {
                return;
            };
            if let Some(w) = writer.as_mut()
                && writeln!(w, "{line}").is_ok()
            {
                *bytes_in_file += (line.len() + 1) as u64;
            }
        };

    loop {
        let msg = receiver.recv_timeout(FLUSH_INTERVAL);
        match msg {
            Ok(record) => {
                write_record(&mut writer, &mut bytes_in_file, record);
                if bytes_in_file >= config.rotate_bytes {
                    if let Some(w) = writer.as_mut() {
                        let _ = w.flush();
                    }
                    drop(writer.take());
                    rotate(&config);
                    writer = open_writer(&config.log_path);
                    bytes_in_file = 0;
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        }
        if last_flush.elapsed() >= FLUSH_INTERVAL {
            if let Some(w) = writer.as_mut() {
                let _ = w.flush();
            }
            last_flush = std::time::Instant::now();
        }
        if shutdown.load(Ordering::Relaxed) {
            // Drain anything already buffered before exiting. The panic hook
            // keeps a sender clone alive for the process lifetime, so we
            // can't wait for channel-disconnect.
            while let Ok(record) = receiver.try_recv() {
                write_record(&mut writer, &mut bytes_in_file, record);
            }
            break;
        }
    }

    if let Some(mut w) = writer.take() {
        let _ = w.flush();
    }
}

fn open_writer(path: &Path) -> Option<BufWriter<File>> {
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .ok()
        .map(BufWriter::new)
}

fn current_size(path: &Path) -> u64 {
    std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

fn rotate(config: &ProbeConfig) {
    let live = &config.log_path;
    if !live.exists() {
        return;
    }
    // Shift events.{n}.jsonl -> events.{n+1}.jsonl up to rotations_kept.
    for i in (1..config.rotations_kept).rev() {
        let from = rotated_path(live, i);
        let to = rotated_path(live, i + 1);
        if from.exists() {
            let _ = std::fs::rename(&from, &to);
        }
    }
    let first = rotated_path(live, 1);
    let _ = std::fs::rename(live, &first);
    // Drop anything beyond the keep window.
    let mut i = config.rotations_kept + 1;
    loop {
        let p = rotated_path(live, i);
        if !p.exists() {
            break;
        }
        let _ = std::fs::remove_file(&p);
        i += 1;
    }
}

fn rotated_path(live: &Path, n: usize) -> PathBuf {
    // events.jsonl -> events.{n}.jsonl
    let parent = live.parent().unwrap_or_else(|| Path::new("."));
    let stem = live
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("events");
    let ext = live.extension().and_then(|s| s.to_str()).unwrap_or("jsonl");
    parent.join(format!("{stem}.{n}.{ext}"))
}

// ---------- panic hook ----------

static PANIC_HOOK_INSTALLED: OnceLock<()> = OnceLock::new();

fn install_panic_hook(sender: SyncSender<LogRecord>, dropped: Arc<AtomicU64>) {
    let already = PANIC_HOOK_INSTALLED.get().is_some();
    if already {
        return;
    }
    PANIC_HOOK_INSTALLED.set(()).ok();

    let prev = std::panic::take_hook();
    let sender = Mutex::new(sender);
    std::panic::set_hook(Box::new(move |info| {
        let mut fields = serde_json::Map::new();
        let payload = info.payload();
        let msg = if let Some(s) = payload.downcast_ref::<&'static str>() {
            (*s).to_string()
        } else if let Some(s) = payload.downcast_ref::<String>() {
            s.clone()
        } else {
            "<non-string panic payload>".to_string()
        };
        fields.insert("message".into(), Value::String(msg));
        if let Some(loc) = info.location() {
            fields.insert("file".into(), Value::String(loc.file().to_string()));
            fields.insert("line".into(), Value::Number(loc.line().into()));
        }
        if let Ok(s) = sender.lock() {
            enqueue(&s, &dropped, make_record("panic", fields));
        }
        prev(info);
    }));
}

// ---------- tracing layer ----------

struct ProbeLayer {
    sender: SyncSender<LogRecord>,
    dropped: Arc<AtomicU64>,
    targets: Vec<String>,
}

impl ProbeLayer {
    fn target_matches(&self, target: &str) -> bool {
        self.targets
            .iter()
            .any(|t| target == t || target.starts_with(&format!("{t}::")))
    }
}

impl<S> Layer<S> for ProbeLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_new_span(&self, attrs: &Attributes<'_>, _id: &Id, _ctx: Context<'_, S>) {
        if !self.target_matches(attrs.metadata().target()) {
            return;
        }
        let mut visitor = FieldVisitor::default();
        attrs.record(&mut visitor);
        let kind = classify(attrs.metadata().target(), attrs.metadata().name());
        let mut fields = visitor.fields;
        fields.insert(
            "span".into(),
            Value::String(attrs.metadata().name().to_string()),
        );
        enqueue(&self.sender, &self.dropped, make_record(&kind, fields));
    }

    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        if !self.target_matches(event.metadata().target()) {
            return;
        }
        let mut visitor = FieldVisitor::default();
        event.record(&mut visitor);
        let kind = classify(event.metadata().target(), event.metadata().name());
        enqueue(
            &self.sender,
            &self.dropped,
            make_record(&kind, visitor.fields),
        );
    }
}

/// Map a `tracing` (target, span/event name) to a kind in our schema.
/// Best-effort — internals are not part of any public Dioxus contract. We
/// pick coarse buckets so unknown names still surface usefully.
fn classify(target: &str, name: &str) -> String {
    let lower_name = name.to_ascii_lowercase();
    let lower_target = target.to_ascii_lowercase();
    if lower_target.contains("signal") || lower_name.contains("signal") {
        if lower_name.contains("write") || lower_name.contains("set") {
            return "signal_write".into();
        }
        if lower_name.contains("read") {
            return "signal_read".into();
        }
        return "signal".into();
    }
    if lower_target.contains("fullstack")
        || lower_name.contains("server_fn")
        || lower_name.contains("server fn")
    {
        return "server_fn".into();
    }
    if lower_target.contains("router") {
        return "route".into();
    }
    if lower_name.contains("render")
        || lower_name.contains("rebuild")
        || lower_target.contains("core")
    {
        return "render".into();
    }
    "event".into()
}

#[derive(Default)]
struct FieldVisitor {
    fields: serde_json::Map<String, Value>,
}

impl Visit for FieldVisitor {
    fn record_str(&mut self, field: &Field, value: &str) {
        self.fields
            .insert(field.name().into(), Value::String(value.to_string()));
    }
    fn record_i64(&mut self, field: &Field, value: i64) {
        self.fields
            .insert(field.name().into(), Value::Number(value.into()));
    }
    fn record_u64(&mut self, field: &Field, value: u64) {
        self.fields
            .insert(field.name().into(), Value::Number(value.into()));
    }
    fn record_f64(&mut self, field: &Field, value: f64) {
        if let Some(n) = serde_json::Number::from_f64(value) {
            self.fields.insert(field.name().into(), Value::Number(n));
        }
    }
    fn record_bool(&mut self, field: &Field, value: bool) {
        self.fields.insert(field.name().into(), Value::Bool(value));
    }
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.fields
            .insert(field.name().into(), Value::String(format!("{value:?}")));
    }
}

// ---------- tests ----------

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    fn tmp_log() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "dioxus-mcp-probe-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("events.jsonl")
    }

    fn read_all(path: &Path) -> String {
        let mut s = String::new();
        File::open(path).unwrap().read_to_string(&mut s).unwrap();
        s
    }

    #[test]
    fn classify_signal_write() {
        assert_eq!(classify("dioxus_signals", "signal_write"), "signal_write");
        assert_eq!(classify("dioxus_signals", "set"), "signal_write");
        assert_eq!(classify("dioxus_router", "navigate"), "route");
        assert_eq!(classify("dioxus_core", "rebuild"), "render");
        assert_eq!(classify("unknown", "thing"), "event");
    }

    #[test]
    fn drop_counter_increments_when_queue_full() {
        let cfg = ProbeConfig {
            log_path: tmp_log(),
            queue_capacity: 1,
            ..ProbeConfig::default()
        };
        let dropped = Arc::new(AtomicU64::new(0));
        let (tx, rx) = sync_channel::<LogRecord>(cfg.queue_capacity);
        // Don't spawn the writer — the rx side stays idle so the channel fills.
        enqueue(&tx, &dropped, make_record("render", Default::default()));
        enqueue(&tx, &dropped, make_record("render", Default::default()));
        enqueue(&tx, &dropped, make_record("render", Default::default()));
        // First enqueue fits the buffer; the next two get dropped.
        assert!(
            dropped.load(Ordering::Relaxed) >= 2,
            "expected drops, got {}",
            dropped.load(Ordering::Relaxed)
        );
        drop(rx);
    }

    #[test]
    fn rotation_rolls_when_over_byte_cap() {
        let log = tmp_log();
        // Large keep window so the test counts every emitted line.
        let cfg = ProbeConfig {
            log_path: log.clone(),
            rotate_bytes: 2_000,
            rotations_kept: 20,
            queue_capacity: 64,
            ..ProbeConfig::default()
        };
        let (tx, rx) = sync_channel::<LogRecord>(64);
        let writer_cfg = cfg.clone();
        let shutdown = Arc::new(AtomicBool::new(false));
        let t = thread::spawn(move || writer_loop(rx, writer_cfg, shutdown));
        let dropped = Arc::new(AtomicU64::new(0));
        for i in 0..50 {
            let mut fields = serde_json::Map::new();
            fields.insert("component".into(), Value::String(format!("Comp{i:03}")));
            fields.insert("padding".into(), Value::String("x".repeat(40)));
            enqueue(&tx, &dropped, make_record("render", fields));
        }
        drop(tx);
        t.join().unwrap();

        assert!(
            rotated_path(&log, 1).exists(),
            "expected at least one rotated file at {:?}",
            rotated_path(&log, 1)
        );

        // Tally lines across the live file and every retained rotation.
        let mut total_lines = read_all(&log).lines().count();
        for i in 1..=cfg.rotations_kept {
            let p = rotated_path(&log, i);
            if p.exists() {
                total_lines += read_all(&p).lines().count();
            }
        }
        assert_eq!(total_lines, 50, "total across live + rotations");
    }

    #[test]
    fn writer_emits_jsonl_schema() {
        let log = tmp_log();
        let cfg = ProbeConfig {
            log_path: log.clone(),
            queue_capacity: 8,
            rotate_bytes: 10 * 1024 * 1024,
            rotations_kept: 1,
            ..ProbeConfig::default()
        };
        let (tx, rx) = sync_channel::<LogRecord>(8);
        let writer_cfg = cfg.clone();
        let shutdown = Arc::new(AtomicBool::new(false));
        let t = thread::spawn(move || writer_loop(rx, writer_cfg, shutdown));
        let dropped = Arc::new(AtomicU64::new(0));
        let mut fields = serde_json::Map::new();
        fields.insert("component".into(), Value::String("Home".into()));
        fields.insert("trigger".into(), Value::String("signal:count".into()));
        enqueue(&tx, &dropped, make_record("render", fields));
        drop(tx);
        t.join().unwrap();
        let line = read_all(&log);
        let parsed: serde_json::Value = serde_json::from_str(line.trim_end()).unwrap();
        assert_eq!(parsed["v"], 1);
        assert_eq!(parsed["kind"], "render");
        assert_eq!(parsed["component"], "Home");
        assert_eq!(parsed["trigger"], "signal:count");
        assert!(parsed["ts"].as_str().unwrap().contains('T'));
    }
}
