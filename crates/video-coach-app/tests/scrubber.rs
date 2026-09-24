//! The transport's scrubber, driven by pointer events on Slint's headless
//! testing backend: a scrub that starts always ends — with a release, at the
//! value it reached — however the pointer leaves it.

use std::cell::RefCell;
use std::rc::Rc;

use slint::platform::{PointerEventButton, WindowEvent};
use slint::{ComponentHandle, LogicalPosition};

slint::slint! {
    import { Scrubber, Mark } from "ui/scrubber.slint";
    export { Mark }

    export component TestWindow inherits Window {
        width: 400px;
        height: 40px;
        out property <bool> scrubbing: scrubber.scrubbing;
        in property <bool> enabled: true;
        in property <bool> wheel-enabled: true;
        in property <[Mark]> marks;
        callback moved(float);
        callback released(float);
        callback scrolled(length, length, bool);

        public pure function mark-x(at: float) -> length {
            return scrubber.x + scrubber.mark-x(at);
        }

        scrubber := Scrubber {
            y: 10px;
            width: 400px;
            height: 20px;
            enabled: root.enabled;
            wheel-enabled: root.wheel-enabled;
            maximum: 100;
            marks: root.marks;
            moved(value) => { root.moved(value); }
            released(value) => { root.released(value); }
            scrolled(dx, dy, shift) => { root.scrolled(dx, dy, shift); }
        }
    }
}

/// The window, and the values its `moved`, `released` and `scrolled`
/// reported.
struct Rig {
    window: TestWindow,
    moved: Rc<RefCell<Vec<f32>>>,
    released: Rc<RefCell<Vec<f32>>>,
    scrolled: Rc<RefCell<Vec<(f32, f32)>>>,
}

impl Rig {
    fn new() -> Rig {
        i_slint_backend_testing::init_no_event_loop();
        let window = TestWindow::new().unwrap();
        let moved: Rc<RefCell<Vec<f32>>> = Rc::default();
        let released: Rc<RefCell<Vec<f32>>> = Rc::default();
        window.on_moved({
            let moved = Rc::clone(&moved);
            move |v| moved.borrow_mut().push(v)
        });
        window.on_released({
            let released = Rc::clone(&released);
            move |v| released.borrow_mut().push(v)
        });
        let scrolled: Rc<RefCell<Vec<(f32, f32)>>> = Rc::default();
        window.on_scrolled({
            let scrolled = Rc::clone(&scrolled);
            move |dx, dy, _shift| scrolled.borrow_mut().push((dx, dy))
        });
        window.show().unwrap();
        Rig {
            window,
            moved,
            released,
            scrolled,
        }
    }

    fn send(&self, event: WindowEvent) {
        self.window.window().dispatch_event(event);
    }

    fn press(&self, x: f32) {
        self.send(WindowEvent::PointerMoved {
            position: LogicalPosition::new(x, 20.0),
        });
        self.send(WindowEvent::PointerPressed {
            position: LogicalPosition::new(x, 20.0),
            button: PointerEventButton::Left,
        });
    }

    fn drag_to(&self, x: f32) {
        self.send(WindowEvent::PointerMoved {
            position: LogicalPosition::new(x, 20.0),
        });
    }

    fn release(&self, x: f32) {
        self.send(WindowEvent::PointerReleased {
            position: LogicalPosition::new(x, 20.0),
            button: PointerEventButton::Left,
        });
    }

    fn scroll(&self, x: f32, delta_y: f32) {
        self.send(WindowEvent::PointerMoved {
            position: LogicalPosition::new(x, 20.0),
        });
        self.send(WindowEvent::PointerScrolled {
            position: LogicalPosition::new(x, 20.0),
            delta_x: 0.0,
            delta_y,
        });
    }
}

#[test]
fn a_click_scrubs_once_and_releases() {
    let rig = Rig::new();
    // The middle of the track: the thumb's 20px centre travels 10..390.
    rig.press(200.0);
    assert!(rig.window.get_scrubbing());
    rig.release(200.0);
    assert!(!rig.window.get_scrubbing());
    assert_eq!(*rig.moved.borrow(), [50.0]);
    assert_eq!(*rig.released.borrow(), [50.0]);
}

#[test]
fn a_drag_scrubs_on_every_move_and_releases_where_it_ended() {
    let rig = Rig::new();
    rig.press(100.0);
    for x in [150.0, 200.0, 250.0] {
        rig.drag_to(x);
        assert!(rig.window.get_scrubbing());
    }
    rig.release(250.0);
    assert!(!rig.window.get_scrubbing());
    let moved = rig.moved.borrow();
    assert!(moved.len() >= 3, "{moved:?}");
    assert!(moved.windows(2).all(|w| w[0] < w[1]), "{moved:?}");
    assert_eq!(*rig.released.borrow(), [*moved.last().unwrap()]);
}

/// On X11 the window gets a leave event when a drag goes past its edge, and
/// Slint cancels the drag there: no pointer-up ever comes.
#[test]
fn a_drag_out_of_the_window_still_releases() {
    let rig = Rig::new();
    rig.press(100.0);
    rig.drag_to(390.0);
    rig.send(WindowEvent::PointerExited);
    assert!(!rig.window.get_scrubbing(), "the scrub is stuck");
    let last = *rig.moved.borrow().last().unwrap();
    assert_eq!(*rig.released.borrow(), [last]);
}

/// Disabled mid-drag (a recording starting): the scrub ends without a
/// release, which the bus would refuse anyway.
#[test]
fn disabling_mid_drag_ends_the_scrub() {
    let rig = Rig::new();
    rig.press(100.0);
    rig.drag_to(200.0);
    rig.window.set_enabled(false);
    rig.drag_to(250.0);
    rig.release(250.0);
    assert!(!rig.window.get_scrubbing());
    assert!(rig.released.borrow().is_empty());
}

/// A mark sits exactly where a click scrubs to its value, from end to end:
/// the marks draw `value-at`'s geometry turned round, and a mark past the end
/// is pinned there.
#[test]
fn a_mark_sits_where_a_click_scrubs_to_its_value() {
    let rig = Rig::new();
    rig.window
        .set_marks(slint::ModelRc::new(slint::VecModel::from(vec![Mark {
            at: 25.0,
            color: slint::Color::from_rgb_u8(255, 255, 255),
        }])));
    for at in [0.0, 25.0, 50.0, 100.0] {
        let x = rig.window.invoke_mark_x(at);
        rig.press(x);
        rig.release(x);
        let got = *rig.released.borrow().last().unwrap();
        assert!((got - at).abs() < 1e-3, "a mark at {at} is at x {x}: {got}");
    }
    assert_eq!(
        rig.window.invoke_mark_x(150.0),
        rig.window.invoke_mark_x(100.0)
    );
    assert_eq!(
        rig.window.invoke_mark_x(0.0),
        10.0,
        "the thumb's half-width in"
    );
}

/// A wheel over the track reports its deltas and scrubs nothing: the skip it
/// turns into is Rust's (`wheel.rs`), and the scrubber's own value must not
/// move under it, or the readout would fight the playhead.
#[test]
fn a_wheel_over_the_track_reports_its_deltas() {
    let rig = Rig::new();
    rig.scroll(200.0, -60.0);
    rig.scroll(200.0, 60.0);
    assert_eq!(*rig.scrolled.borrow(), [(0.0, -60.0), (0.0, 60.0)]);
    assert!(rig.moved.borrow().is_empty());
    assert!(!rig.window.get_scrubbing());
}

/// Mid-drag the pointer is already saying where to go, so the wheel keeps out
/// of it. Gated off (nothing loaded, a preview open) it does nothing at all —
/// and that gate is its own, so a recording, which disables the drag, leaves
/// the wheel working.
#[test]
fn the_wheel_keeps_out_of_a_drag_and_obeys_its_own_gate() {
    let rig = Rig::new();
    rig.press(100.0);
    rig.scroll(100.0, -60.0);
    assert!(rig.scrolled.borrow().is_empty());
    rig.release(100.0);

    rig.window.set_wheel_enabled(false);
    rig.scroll(200.0, -60.0);
    assert!(rig.scrolled.borrow().is_empty());

    // The drag's own gate is not the wheel's: a recording disables one.
    rig.window.set_wheel_enabled(true);
    rig.window.set_enabled(false);
    rig.scroll(200.0, -60.0);
    assert_eq!(*rig.scrolled.borrow(), [(0.0, -60.0)]);
}
