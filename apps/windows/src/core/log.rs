//! Log to stderr and a file, and (while connected) to the host, like the Mac client does, so
//! everything ends up in the host's log.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

type Sink = Box<dyn Fn(&str) + Send + Sync>;

struct State {
    file: Option<File>,
    sink: Option<Sink>,
}

static STATE: Mutex<State> = Mutex::new(State { file: None, sink: None });
const MAX_BYTES: u64 = 20_000_000;

/// Opens <dir>/<name>.log for appending, moving a full one to <name>.old.log first.
pub fn setup(dir: PathBuf, name: &str) {
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join(format!("{name}.log"));
    if std::fs::metadata(&path).map(|m| m.len() > MAX_BYTES).unwrap_or(false) {
        let _ = std::fs::rename(&path, dir.join(format!("{name}.old.log")));
    }
    let file = OpenOptions::new().create(true).append(true).open(&path).ok();
    STATE.lock().unwrap().file = file;
}

/// Every line goes here too (nil to stop). Used to forward lines to the host.
pub fn set_sink(sink: Option<Sink>) {
    STATE.lock().unwrap().sink = sink;
}

pub fn info(message: &str) {
    let line = format!("{} {message}\n", timestamp());
    let mut state = STATE.lock().unwrap();
    eprint!("{line}");
    if let Some(f) = state.file.as_mut() {
        let _ = f.write_all(line.as_bytes());
    }
    if let Some(sink) = state.sink.as_ref() {
        sink(message);
    }
}

/// UTC "2026-10-25 13:04:05.123" (no time zone database needed).
fn timestamp() -> String {
    let d = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    let secs = d.as_secs() as i64;
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Civil date from days since 1970 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + if month <= 2 { 1 } else { 0 };
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02}.{:03}Z",
        rem / 3600,
        rem / 60 % 60,
        rem % 60,
        d.subsec_millis()
    )
}

#[macro_export]
macro_rules! logi {
    ($($arg:tt)*) => { $crate::core::log::info(&format!($($arg)*)) };
}
