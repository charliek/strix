use std::sync::mpsc::RecvTimeoutError;
use std::time::Duration;

use strix::watch;
use tempfile::tempdir;

/// End-to-end: a real file change under the watched root produces a signal.
/// Timing-based (FS watcher + debounce) but with a generous timeout, so a
/// single write should always be observed well within it.
#[test]
fn watcher_signals_on_a_file_change() {
    let dir = tempdir().expect("tempdir");
    let rx = watch::spawn(dir.path().to_path_buf()).expect("spawn watcher");
    // Let the watch register before touching files.
    std::thread::sleep(Duration::from_millis(300));
    std::fs::write(dir.path().join("hello.txt"), "hi").expect("write");

    match rx.recv_timeout(Duration::from_secs(5)) {
        Ok(()) => {}
        Err(RecvTimeoutError::Timeout) => panic!("watcher sent no signal within 5s"),
        Err(err) => panic!("watch channel error: {err}"),
    }
}
