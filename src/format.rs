use std::time::Duration;

pub fn duration(d: Duration) -> String {
    let seconds = d.as_secs_f64();
    if seconds < 0.001 {
        format!("{:.3}µs", seconds * 1_000_000.0)
    } else if seconds < 1.0 {
        format!("{:.3}ms", seconds * 1_000.0)
    } else if seconds < 60.0 {
        format!("{:.3}s", seconds)
    } else {
        let minutes = (seconds / 60.0).floor();
        format!("{minutes:.0}m{:.3}s", seconds - minutes * 60.0)
    }
}

pub fn size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;

    let bytes = bytes as f64;
    if bytes < KB {
        format!("{bytes:.0} B")
    } else if bytes < MB {
        format!("{:.1} KB", bytes / KB)
    } else if bytes < GB {
        format!("{:.1} MB", bytes / MB)
    } else {
        format!("{:.2} GB", bytes / GB)
    }
}

pub fn speed(bytes: u64, d: Duration) -> String {
    let seconds = d.as_secs_f64();
    if seconds <= 0.0 || bytes == 0 {
        return "-".to_string();
    }
    format!("{}/s", size((bytes as f64 / seconds) as u64))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_durations() {
        assert_eq!(duration(Duration::from_nanos(145_480)), "145.480µs");
        assert_eq!(duration(Duration::from_micros(1341)), "1.341ms");
        assert_eq!(duration(Duration::from_millis(41_518)), "41.518s");
        assert_eq!(duration(Duration::from_secs(90)), "1m30.000s");
    }

    #[test]
    fn formats_sizes() {
        assert_eq!(size(512), "512 B");
        assert_eq!(size(1024 * 1024 * 3), "3.0 MB");
        assert_eq!(size(1024 * 1024 * 1024 * 2), "2.00 GB");
    }
}
