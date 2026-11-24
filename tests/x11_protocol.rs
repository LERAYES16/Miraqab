//! The raw X11 client against a real X server (Xvfb) and a real window.
#![cfg(target_os = "linux")]
mod common;
use common::*;
use miraqab::x11::{WinState, X11};

#[test]
fn lists_windows_monitors_and_tracks_resize_and_close() {
    if !x11_tools_available() {
        return;
    }
    let xs = XServer::start("1024x768");
    let mut win = TestWindow::open(&xs.display, "320x240", &["1:480x360"], 30);
    let mut x = X11::connect_to(&xs.display).expect("connect");

    assert_eq!((x.screen.w, x.screen.h), (1024, 768));
    assert_eq!(x.root_size().unwrap().w, 1024);

    // the window shows up in the list with its title and size
    let list = x.list_windows().unwrap();
    let w = list.iter().find(|w| w.id == win.id).expect("test window listed");
    assert_eq!(w.title, "Miraqab Test Window");
    assert_eq!((w.rect.w, w.rect.h), (320, 240));
    assert!(!w.hidden);

    // at least one monitor with the screen size
    let mons = x.monitors().unwrap();
    assert!(!mons.is_empty());
    assert_eq!((mons[0].rect.w, mons[0].rect.h), (1024, 768));

    // the window resizes itself after 1 s; we must see the new size
    std::thread::sleep(std::time::Duration::from_millis(1500));
    match x.window_state(win.id) {
        WinState::Visible(r) => assert_eq!((r.w, r.h), (480, 360)),
        other => panic!("expected Visible, got {:?}", other),
    }

    // closing the window is reported as Gone, never as another window
    win.close();
    std::thread::sleep(std::time::Duration::from_millis(300));
    assert_eq!(x.window_state(win.id), WinState::Gone);
    // an id that never existed
    assert_eq!(x.window_state(0x7fffff0), WinState::Gone);
}

#[test]
fn unreachable_display_is_a_clean_error() {
    let e = X11::connect_to(":59").err().expect("must fail");
    assert!(!e.to_string().is_empty());
    assert!(X11::connect_to("somehost:0").is_err());
    assert!(X11::connect_to("garbage").is_err());
}
