use super::*;

const FONTS: (&str, &str, &str) = ("ui", "ai", "mono");

#[test]
fn tool_measurement_ranges_merge_with_live_text_and_reset() {
    let mut measurements = TranscriptMeasurements::default();
    measurements.take_range(100, 102, (0, 10), FONTS);
    measurements.dirty_from = Some(98);
    assert_eq!(
        measurements.take_range(100, 102, (0, 11), FONTS),
        Some(98..102)
    );
    assert_eq!(measurements.take_range(100, 102, (0, 11), FONTS), None);
    measurements.dirty_from = Some(101);
    assert_eq!(
        measurements.take_range(100, 102, (0, 12), FONTS),
        Some(99..102)
    );
    measurements.dirty_from = Some(99);
    measurements.dirty = true;
    assert_eq!(
        measurements.take_range(100, 102, (0, 12), FONTS),
        Some(0..102)
    );
    assert_eq!(measurements.take_range(100, 102, (0, 12), FONTS), None);
}

#[gpui::test]
fn streamed_tool_arguments_preserve_history_measurements(cx: &mut gpui::TestAppContext) {
    let (panel, vcx) = cx.add_window_view(|_, cx| {
        let mut panel = new_panel(cx);
        panel.status = "running_tools".into();
        panel.items.push(Item::Tool {
            call_id: "streamed-tool".into(),
            name: "bash".into(),
            input: r#"{"intent":"Build the project","command":""#.into(),
            output: String::new(),
            done: false,
            error: None,
        });
        panel
    });
    vcx.run_until_parked();
    let before = panel.read_with(vcx, |panel, _| {
        panel.transcript_measurements.remeasured_rows
    });
    for chunk in 0..20 {
        panel.update(vcx, |panel, cx| {
            panel.apply(
                &ApiEvent::ToolInputDelta {
                    session_id: panel.session_id.clone(),
                    // Exercise both modern and legacy lookup without changing the row.
                    call_id: if chunk % 2 == 0 { "streamed-tool" } else { "" }.into(),
                    delta: "more arguments ".into(),
                },
                cx,
            );
        });
        vcx.run_until_parked();
    }
    panel.read_with(vcx, |panel, _| {
        let remeasured = panel.transcript_measurements.remeasured_rows - before;
        assert!(remeasured >= 20, "changed tool row must be remeasured");
        assert!(
            remeasured <= 60,
            "20 chunks remeasured {remeasured} rows, including stable history"
        );
        assert!(
            panel
                .tool_input("streamed-tool")
                .unwrap()
                .ends_with(&"more arguments ".repeat(20))
        );
    });
    assert!(vcx.debug_bounds("tool-summary").is_some());
}

#[gpui::test]
fn batched_tool_deltas_resolve_presentation_rows_after_merging(cx: &mut gpui::TestAppContext) {
    let (panel, vcx) = cx.add_window_view(|_, cx| {
        let mut panel = new_panel(cx);
        panel.status = "running_tools".into();
        // Consecutive reasoning entries occupy only one presentation row.
        panel.items.push(Item::Reasoning("First thought".into()));
        panel.items.push(Item::Reasoning("Second thought".into()));
        for id in ["first", "second"] {
            panel.items.push(Item::Tool {
                call_id: id.into(),
                name: "bash".into(),
                input: r#"{"intent":"Stable summary","command":""#.into(),
                output: String::new(),
                done: false,
                error: None,
            });
        }
        panel
    });
    vcx.run_until_parked();
    let (before, suffix) = panel.read_with(vcx, |panel, _| {
        let rows = &panel.transcript_derived.as_ref().unwrap().rows;
        let first = rows
            .iter()
            .position(|row| matches!(row.source, TranscriptRowSource::Settled(102)))
            .unwrap();
        (
            panel.transcript_measurements.remeasured_rows,
            panel.transcript_row_count - first,
        )
    });
    panel.update(vcx, |panel, cx| {
        for id in ["second", "first", "second", "first"] {
            panel.apply(
                &ApiEvent::ToolInputDelta {
                    session_id: panel.session_id.clone(),
                    call_id: id.into(),
                    delta: "chunk ".into(),
                },
                cx,
            );
        }
    });
    vcx.run_until_parked();
    panel.read_with(vcx, |panel, _| {
        assert_eq!(
            panel.transcript_measurements.remeasured_rows - before,
            suffix
        );
        assert!(panel.tool_input("first").unwrap().ends_with("chunk chunk "));
        assert!(
            panel
                .tool_input("second")
                .unwrap()
                .ends_with("chunk chunk ")
        );
    });
}

#[test]
fn measurement_plan_reuses_history_and_bounds_streaming_work() {
    let mut measurements = TranscriptMeasurements::default();
    assert_eq!(
        measurements.take_range(10_000, 10_002, (0, 10), FONTS),
        Some(0..10_002)
    );
    assert_eq!(
        measurements.take_range(10_000, 10_002, (0, 10), FONTS),
        None
    );
    for length in 11..111 {
        assert_eq!(
            measurements.take_range(10_000, 10_002, (0, length), FONTS),
            Some(9_999..10_002)
        );
    }
    assert_eq!(measurements.remeasured_rows, 10_002 + 300);
}

#[test]
fn measurement_plan_invalidates_structure_fonts_and_explicit_changes() {
    let mut measurements = TranscriptMeasurements::default();
    measurements.take_range(10, 12, (10, 20), FONTS);
    measurements.dirty = true;
    assert_eq!(
        measurements.take_range(10, 12, (10, 20), FONTS),
        Some(0..12)
    );
    // A hidden todo or an image insertion may change item identity without
    // changing the visible row count. Preserve the conservative fallback.
    assert_eq!(
        measurements.take_range(11, 12, (10, 20), FONTS),
        Some(0..12)
    );
    assert_eq!(measurements.take_range(11, 11, (0, 20), FONTS), Some(0..11));
    assert_eq!(
        measurements.take_range(11, 11, (0, 20), ("new", "ai", "mono")),
        Some(0..11)
    );
    assert_eq!(measurements.take_range(0, 0, (0, 0), FONTS), None);
    assert_eq!(measurements.take_range(0, 2, (0, 1), FONTS), Some(0..2));
    assert_eq!(measurements.take_range(0, 2, (0, 2), FONTS), Some(0..2));
}

fn new_panel(cx: &mut Context<Panel>) -> Panel {
    let mut panel = Panel::new(
        "measurement-test".into(),
        None,
        None,
        crate::harness::spawn_inert(),
        cx,
    );
    *panel.items = (0..100)
        .map(|index| Item::Assistant(format!("History {index}\n\nSecond paragraph.")))
        .collect();
    panel
}

#[gpui::test]
fn unchanged_panel_repaints_do_not_discard_history_measurements(cx: &mut gpui::TestAppContext) {
    let (panel, vcx) = cx.add_window_view(|_, cx| new_panel(cx));
    vcx.run_until_parked();
    let before = panel.read_with(vcx, |panel, _| {
        panel.transcript_measurements.remeasured_rows
    });
    assert!(before >= 100);
    for _ in 0..20 {
        panel.update(vcx, |_, cx| cx.notify());
        vcx.run_until_parked();
    }
    assert_eq!(
        panel.read_with(vcx, |panel, _| panel
            .transcript_measurements
            .remeasured_rows),
        before
    );
    assert!(vcx.debug_bounds("transcript").is_some());
}

#[gpui::test]
fn live_text_growth_remeasures_only_suffix_and_updates_real_geometry(
    cx: &mut gpui::TestAppContext,
) {
    let (panel, vcx) = cx.add_window_view(|_, cx| new_panel(cx));
    let handle = vcx.update(|window, _| window.window_handle());
    vcx.simulate_window_resize(handle, gpui::size(px(600.), px(500.)));
    panel.update(vcx, |panel, cx| {
        panel.apply(
            &ApiEvent::TextDelta {
                message_id: None,
                session_id: panel.session_id.clone(),
                text: "Initial reply.".into(),
            },
            cx,
        );
    });
    vcx.run_until_parked();
    let before = panel.read_with(vcx, |panel, _| {
        (
            panel.transcript_measurements.remeasured_rows,
            panel
                .transcript_list
                .bounds_for_item(100)
                .unwrap()
                .size
                .height,
        )
    });
    panel.update(vcx, |panel, cx| {
        panel.apply(
            &ApiEvent::TextDelta {
                message_id: None,
                session_id: panel.session_id.clone(),
                text: "\n\nAdditional paragraph.".repeat(5),
            },
            cx,
        );
    });
    vcx.run_until_parked();
    panel.read_with(vcx, |panel, _| {
        assert_eq!(panel.transcript_measurements.remeasured_rows - before.0, 3);
        assert!(
            panel
                .transcript_list
                .bounds_for_item(100)
                .unwrap()
                .size
                .height
                > before.1
        );
    });
}

#[gpui::test]
fn tools_selection_and_recovery_still_invalidate_existing_rows(cx: &mut gpui::TestAppContext) {
    let (panel, vcx) = cx.add_window_view(|_, cx| {
        let mut panel = new_panel(cx);
        panel.items.push(Item::Tool {
            call_id: "test-tool".into(),
            name: "bash".into(),
            input: "{}".into(),
            output: String::new(),
            done: false,
            error: None,
        });
        panel
    });
    vcx.run_until_parked();
    let before = panel.read_with(vcx, |panel, _| {
        panel.transcript_measurements.remeasured_rows
    });
    panel.update(vcx, |panel, cx| {
        panel.apply(
            &ApiEvent::ToolDone {
                session_id: panel.session_id.clone(),
                call_id: "test-tool".into(),
                name: "bash".into(),
                output: "result".into(),
                error: Some("Failure detail".into()),
            },
            cx,
        );
    });
    vcx.run_until_parked();
    let after_tool = panel.read_with(vcx, |panel, _| {
        assert!(panel.transcript_measurements.remeasured_rows >= before + 101);
        panel.transcript_measurements.remeasured_rows
    });
    let selection = panel.read_with(vcx, |panel, _| panel.transcript_selection.clone());
    selection.update(vcx, |_, cx| cx.notify());
    vcx.run_until_parked();
    panel.read_with(vcx, |panel, _| {
        assert!(panel.transcript_measurements.remeasured_rows >= after_tool + 101)
    });
    panel.update(vcx, |panel, _| {
        panel.transcript_measurements.dirty = false;
        panel.recover_response("History 99\n\nSecond paragraph. More restored text.");
        assert!(
            panel.transcript_measurements.dirty,
            "same-count recovery must invalidate old heights"
        );
    });
}

#[gpui::test]
fn width_change_remeasures_visible_rows_without_content_invalidation(
    cx: &mut gpui::TestAppContext,
) {
    let (panel, vcx) = cx.add_window_view(|_, cx| {
        let mut panel = new_panel(cx);
        *panel.items = vec![Item::Assistant(
            "A wrapping paragraph with several words. ".repeat(20),
        )];
        panel
    });
    let handle = vcx.update(|window, _| window.window_handle());
    vcx.simulate_window_resize(handle, gpui::size(px(900.), px(700.)));
    vcx.run_until_parked();
    let before = panel.read_with(vcx, |panel, _| {
        panel
            .transcript_list
            .bounds_for_item(0)
            .unwrap()
            .size
            .height
    });
    vcx.simulate_window_resize(handle, gpui::size(px(400.), px(700.)));
    vcx.run_until_parked();
    panel.read_with(vcx, |panel, _| {
        assert!(
            panel
                .transcript_list
                .bounds_for_item(0)
                .unwrap()
                .size
                .height
                > before,
            "GPUI's width invalidation must still update wrapping and row height"
        );
    });
}
