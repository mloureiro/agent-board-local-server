use std::time::{SystemTime, UNIX_EPOCH};

fn local_now() -> libc::tm {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs()) as libc::time_t;
    // SAFETY: localtime_r only writes into the struct we pass; both pointers are valid for the call.
    unsafe {
        let mut parts: libc::tm = std::mem::zeroed();
        libc::localtime_r(&seconds, &mut parts);
        parts
    }
}

pub fn clock() -> String {
    let now = local_now();
    format!("{:02}:{:02}", now.tm_hour, now.tm_min)
}

pub fn timestamp() -> String {
    let now = local_now();
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        now.tm_year + 1900,
        now.tm_mon + 1,
        now.tm_mday,
        now.tm_hour,
        now.tm_min,
        now.tm_sec
    )
}

pub fn compact() -> String {
    let now = local_now();
    format!(
        "{:04}{:02}{:02}-{:02}{:02}{:02}",
        now.tm_year + 1900,
        now.tm_mon + 1,
        now.tm_mday,
        now.tm_hour,
        now.tm_min,
        now.tm_sec
    )
}
