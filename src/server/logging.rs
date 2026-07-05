use std::time::{SystemTime, UNIX_EPOCH};

pub fn log(log_level: u16, required_level: u16, scope: &str, message: &str) {
    if log_level >= required_level {
        log_always(scope, message);
    }
}

pub fn log_always(scope: &str, message: &str) {
    eprintln!("{} [{scope}] {message}", timestamp());
}

fn timestamp() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let seconds = now.as_secs();
    let millis = now.subsec_millis();

    format!("{seconds}.{millis:03}")
}
