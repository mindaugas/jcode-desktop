use super::*;
use gpui::TestAppContext;

#[gpui::test]
fn workspace_activation_follows_a_complete_loading_frame(cx: &mut TestAppContext) {
    let activated = Rc::new(Cell::new(0));
    let handle = cx.update(|cx| {
        let handle = cx
            .open_window(
                WindowOptions {
                    inactive_frame_interval: None,
                    ..Default::default()
                },
                |_, cx| cx.new(|_| HostFallback),
            )
            .unwrap();
        handle
            .update(cx, {
                let activated = activated.clone();
                move |_, window, _| {
                    after_loading_frame(window, move |_| activated.set(activated.get() + 1))
                }
            })
            .unwrap();
        handle
    });
    let frame = |cx: &mut TestAppContext| {
        handle
            .update(cx, |_, window, cx| window.simulate_next_frame(cx))
            .unwrap();
    };
    assert_eq!(activated.get(), 0);
    frame(cx);
    assert_eq!(activated.get(), 0, "first frame must keep the loading root");
    frame(cx);
    assert_eq!(activated.get(), 1);
    frame(cx);
    assert_eq!(activated.get(), 1, "activation must be exactly once");
}
