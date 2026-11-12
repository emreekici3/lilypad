//! Runs the daemon against a fake window system with a real `lilyrc`,
//! which configures it through the real `lily` binary and socket.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use lilypad::geometry::{Rect, WinId};
use lilypad::platform::{Backend, WindowInfo};

#[derive(Clone, Default)]
struct Fake(Arc<Mutex<Vec<WindowInfo>>>);

impl Backend for Fake {
    fn windows(&mut self) -> Vec<WindowInfo> {
        self.0.lock().unwrap().clone()
    }
    fn focused_window(&mut self) -> Option<WinId> {
        self.0.lock().unwrap().last().map(|w| w.id)
    }
    fn tiling_area(&mut self) -> Rect {
        Rect::new(0, 0, 1000, 600)
    }
    fn set_frame(&mut self, id: WinId, frame: Rect) {
        if let Some(w) = self.0.lock().unwrap().iter_mut().find(|w| w.id == id) {
            w.frame = frame;
        }
    }
    fn focus(&mut self, _id: WinId) {}
}

#[test]
fn lilyrc_configures_the_daemon() {
    let dir = std::env::temp_dir().join(format!("lily-rc-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let socket = dir.join("s.sock");
    let rc = dir.join("lilyrc");
    std::fs::write(
        &rc,
        "lily config step 20\nlily config gap 0\nlily config animation off\nlily rule grid 1:5:0:0:3:1 Notes\n",
    )
    .unwrap();

    // The script finds `lily` on PATH and the socket through the environment.
    let bin_dir = std::path::Path::new(env!("CARGO_BIN_EXE_lily"))
        .parent()
        .unwrap()
        .to_owned();
    let path = std::env::join_paths(std::iter::once(bin_dir).chain(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    )))
    .unwrap();
    unsafe {
        std::env::set_var("PATH", path);
        std::env::set_var("LILY_SOCKET", &socket);
    }

    let fake = Fake::default();
    {
        let mut w = fake.0.lock().unwrap();
        for (id, app) in [(1, "Terminal"), (2, "Safari"), (3, "Notes")] {
            w.push(WindowInfo {
                id,
                frame: Rect::new(50, 50, 200, 200),
                app: app.into(),
                resizable: true,
                movable: true,
            });
        }
    }
    let server = lilypad::ipc::Server::bind(&socket).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let daemon = {
        let (fake, stop) = (fake.clone(), stop.clone());
        std::thread::spawn(move || lilypad::daemon::run(fake, Some(rc), server, stop))
    };

    let frame = |id: WinId| {
        fake.0
            .lock()
            .unwrap()
            .iter()
            .find(|w| w.id == id)
            .unwrap()
            .frame
    };
    let lily = |args: &[&str]| {
        std::process::Command::new(env!("CARGO_BIN_EXE_lily"))
            .args(args)
            .output()
            .unwrap()
    };
    // Up once the script has run and the daemon had its first look.
    let deadline = Instant::now() + Duration::from_secs(10);
    while lily(&["query"]).stdout.is_empty() {
        assert!(Instant::now() < deadline, "the daemon never looked");
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(
        String::from_utf8_lossy(&lily(&["config", "step"]).stdout),
        "20\n"
    );
    assert_eq!(frame(2), Rect::new(50, 50, 200, 200), "nothing moved");

    // A new window with a rule goes to its place.
    fake.0.lock().unwrap().push(WindowInfo {
        id: 4,
        frame: Rect::new(50, 50, 200, 200),
        app: "Notes".into(),
        resizable: true,
        movable: true,
    });
    // Commands make the daemon look at the windows.
    assert!(lily(&["query"]).status.success());
    let deadline = Instant::now() + Duration::from_secs(10);
    while frame(4) != Rect::new(0, 0, 600, 600) {
        assert!(
            Instant::now() < deadline,
            "the rule never applied: {:?}",
            frame(4)
        );
        assert!(lily(&["query"]).status.success());
        std::thread::sleep(Duration::from_millis(20));
    }

    // A bad value is refused, like for a user in a terminal.
    let out = lily(&["config", "step", "oops"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("not a valid number"));

    stop.store(true, Ordering::Relaxed);
    daemon.join().unwrap();
    std::fs::remove_dir_all(dir).unwrap();
}
