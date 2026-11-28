//! Camera movement is a prepaint offset, not a change to layout constraints.
//!
//! Like a scroll offset, it must move descendants' hitboxes, text geometry and
//! canvases together. Descendants still prepaint and paint at their new bounds:
//! this deliberately does not replay a scene with stale absolute coordinates.

use gpui::{
    AnyElement, App, Bounds, Element, ElementId, GlobalElementId, InspectorElementId, IntoElement,
    LayoutId, Pixels, Window, point, px,
};

pub(super) struct CameraOffset {
    child: AnyElement,
    x: Pixels,
}

pub(super) fn camera_offset(child: impl IntoElement, x: Pixels) -> CameraOffset {
    CameraOffset {
        child: child.into_any_element(),
        x,
    }
}

impl IntoElement for CameraOffset {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

impl Element for CameraOffset {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }
    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        (self.child.request_layout(window, cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        window.with_element_offset(point(self.x, px(0.)), |window| {
            self.child.prepaint(window, cx);
        });
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        self.child.paint(window, cx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{Context, Render, StyledText, TextLayout, div, prelude::*};
    use std::{cell::Cell, rc::Rc};

    struct Fixture {
        offset: f32,
        clicks: Rc<Cell<usize>>,
        layout: TextLayout,
    }

    impl Render for Fixture {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let text = StyledText::new("moving text");
            self.layout = text.layout().clone();
            let clicks = self.clicks.clone();
            div()
                .relative()
                .w(px(220.))
                .h(px(100.))
                .overflow_hidden()
                .child(camera_offset(
                    div()
                        .absolute()
                        .left(px(0.))
                        .top(px(0.))
                        .w(px(400.))
                        .h(px(100.))
                        .child(
                            div()
                                .id("moving-button")
                                .debug_selector(|| "moving-button".into())
                                .absolute()
                                .left(px(120.))
                                .top(px(10.))
                                .w(px(80.))
                                .h(px(40.))
                                .on_mouse_down(gpui::MouseButton::Left, move |_, _, _| {
                                    clicks.set(clicks.get() + 1);
                                })
                                .child(text),
                        ),
                    px(self.offset),
                ))
        }
    }

    #[gpui::test]
    fn camera_offset_moves_geometry_and_hitboxes_without_layout_writes(
        cx: &mut gpui::TestAppContext,
    ) {
        let clicks = Rc::new(Cell::new(0));
        let (view, vcx) = cx.add_window_view(|_, _| Fixture {
            offset: 0.,
            clicks: clicks.clone(),
            layout: TextLayout::default(),
        });
        vcx.run_until_parked();
        let original = vcx.debug_bounds("moving-button").unwrap();
        let original_text = view.read_with(vcx, |view, _| view.layout.bounds());
        assert_eq!(original.left(), px(120.));
        vcx.update(|window, _| window.reset_layout_stats());
        view.update(vcx, |view, cx| {
            view.offset = -90.;
            cx.notify();
        });
        vcx.run_until_parked();
        let shifted = vcx.debug_bounds("moving-button").unwrap();
        let shifted_text = view.read_with(vcx, |view, _| view.layout.bounds());
        assert_eq!(shifted.left(), px(30.));
        assert_eq!(shifted.size, original.size);
        assert_eq!(shifted_text.left(), original_text.left() - px(90.));
        let stats = vcx.update(|window, _| window.layout_stats());
        assert_eq!(
            stats.style_writes, 0,
            "camera-only pan dirtied layout: {stats:?}"
        );
        assert_eq!(
            stats.measure_calls, 0,
            "camera-only pan remeasured text: {stats:?}"
        );
        vcx.simulate_click(shifted.center(), gpui::Modifiers::default());
        assert_eq!(clicks.get(), 1, "new pointer target must follow camera");
        vcx.simulate_click(original.center(), gpui::Modifiers::default());
        assert_eq!(clicks.get(), 1, "old pointer target must not remain active");
        view.update(vcx, |view, cx| {
            view.offset = -170.;
            cx.notify();
        });
        vcx.run_until_parked();
        vcx.simulate_click(point(px(10.), px(30.)), gpui::Modifiers::default());
        assert_eq!(
            clicks.get(),
            2,
            "partially clipped target remains interactive"
        );
        vcx.simulate_click(point(px(40.), px(30.)), gpui::Modifiers::default());
        assert_eq!(
            clicks.get(),
            2,
            "outside moved bounds must not receive input"
        );
    }
}
