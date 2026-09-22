//! The transport's scrubber, driven by pointer events on Slint's headless
//! testing backend: a scrub that starts always ends — with a release, at the
//! value it reached — however the pointer leaves it.

use std::cell::RefCell;
use std::rc::Rc;

use slint::platform::{PointerEventButton, WindowEvent};
use slint::{ComponentHandle, LogicalPosition};

slint::slint! {
    import { Scrubber } from "ui/scrubber.slint";

    export component TestWindow inherits Window {
        width: 400px;
        height: 40px;
        out property <bool> scrubbing: scrubber.scrubbing;
        in property <bool> enabled: true;
        callback moved(float);
        callback released(float);

        scrubber := Scrubber {
            y: 10px;
            width: 400px;
            height: 20px;
            enabled: root.enabled;
            maximum: 100;
            moved(value) => { root.moved(value); }
            released(value) => { root.released(value); }
        }
    }
}

/// The window, and the values its `moved` and `released` reported.
struct Rig {
    window: TestWindow,
    moved: Rc<RefCell<Vec<f32>>>,
    released: Rc<RefCell<Vec<f32>>>,
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
        window.show().unwrap();
        Rig {
            window,
            moved,
            released,
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
