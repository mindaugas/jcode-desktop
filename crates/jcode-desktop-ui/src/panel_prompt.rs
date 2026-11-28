use super::*;
use std::{cell::Cell, rc::Rc};

// Preserve the transcript row's inter-message spacing independently of the header.
pub(super) const PROMPT_TOP_PADDING: f32 = 10.;
const COLLAPSED_PROMPT_LINES: usize = 6;
const COLLAPSED_PROMPT_MAX_HEIGHT: f32 = 140.;

/// Bound both long pasted paragraphs and newline-heavy snippets. Return a
/// source prefix, never alter the actual prompt sent to the harness or stored.
pub(super) fn compact_prompt(text: &str) -> Option<&str> {
    let mut lines = 1;
    for (characters, (byte, ch)) in text.char_indices().enumerate() {
        if characters == 600 || (ch == '\n' && lines == 6) {
            return Some(&text[..byte]);
        }
        if ch == '\n' {
            lines += 1;
        }
    }
    None
}

pub(super) fn fixture_items() -> Vec<Item> {
    vec![
        Item::User("A longer first line with a short second line\nShort second line.".into()),
        Item::User("Short first.\nA much longer second line that grows the rounded outline outward.".into()),
        Item::User("Please make this naturally wrapped prompt fit closely around every rendered line, keeping **bold text**, `inline code`, and [links](https://example.com) selectable without filling the unused space after the final words. Fin.".into()),
        Item::User("Unicode stays aligned: café 日本語 🦀\nSmall.\nA longer third line to exercise both inward and outward curves.".into()),
        Item::User("A separate paragraph stays separate.\n\nTiny.".into()),
        Item::User(super::voice::tag::wrap("Can you fix the flaky sidebar test and then rerun it")),
        Item::User("Please review this pasted specification and preserve the complete original prompt.\n".repeat(30)),
    ]
}

#[derive(Clone, Copy, Debug)]
pub(super) struct VisibleRows {
    first: usize,
    last: usize,
    first_top: gpui::Pixels,
    next_prompt_top: Option<(usize, gpui::Pixels)>,
}

/// The list's logical offset is an end sentinel while following the tail, not
/// the first visible row. Observe real painted bounds instead of that offset.
pub(super) fn visibility_marker(
    row: usize,
    is_prompt: bool,
    first_visible: Rc<Cell<Option<VisibleRows>>>,
    list: ListState,
) -> gpui::AnyElement {
    gpui::canvas(
        |_, _, _| (),
        move |bounds, _, _, _| {
            let viewport = list.viewport_bounds();
            if bounds.bottom() > viewport.top() && bounds.top() < viewport.bottom() {
                let top = bounds.top() - viewport.top();
                let mut visible = first_visible.get().unwrap_or(VisibleRows {
                    first: row,
                    last: row,
                    first_top: top,
                    next_prompt_top: None,
                });
                if row <= visible.first {
                    visible.first = row;
                    visible.first_top = top;
                }
                let card_top = top + px(PROMPT_TOP_PADDING);
                if is_prompt
                    && card_top > px(0.)
                    && visible.next_prompt_top.is_none_or(|(next, _)| row < next)
                {
                    visible.next_prompt_top = Some((row, card_top));
                }
                visible.last = visible.last.max(row);
                first_visible.set(Some(visible));
            }
        },
    )
    .absolute()
    .top_0()
    .left_0()
    .size_full()
    .into_any_element()
}

/// The header always represents the latest genuine user turn, even before the
/// first list paint, while browsing history, or below a tall attachment.
fn prompt_for_viewport(
    prompt_rows: &[(usize, usize)],
    _visible: Option<VisibleRows>,
    _at_live_end: bool,
) -> Option<usize> {
    prompt_rows.last().map(|(_, index)| *index)
}

pub(super) fn is_pinnable_prompt(item: &Item) -> bool {
    let Item::User(text) = item else {
        return false;
    };
    let text = text.trim();
    // Background notifications can arrive in restored history as user-role
    // markdown. Keep them in the transcript, never in the pinned reminder.
    // Auto-poke and quality-gate continuations are not prompts either.
    !text.is_empty()
        && !jcode_sdk::todo::is_auto_poke_message(text)
        && ![
            "**Background task** `",
            "**Background task started** `",
            "**Background task progress** `",
            "**Background task stalled** `",
        ]
        .iter()
        .any(|prefix| text.starts_with(prefix))
}

/// Count user turns rather than transcript rows. Tool output, assistant text,
/// todo snapshots and restored background notices must not age prompt colors.
fn prompt_distance(items: &[Item], index: usize) -> Option<usize> {
    items.get(index).filter(|item| is_pinnable_prompt(item))?;
    Some(
        items
            .iter()
            .skip(index + 1)
            .filter(|item| is_pinnable_prompt(item))
            .count(),
    )
}

/// Oldest genuine prompt is one. Later arrivals never renumber history.
fn prompt_number(items: &[Item], index: usize) -> Option<usize> {
    items.get(index).filter(|item| is_pinnable_prompt(item))?;
    Some(
        items[..=index]
            .iter()
            .filter(|item| is_pinnable_prompt(item))
            .count(),
    )
}

impl Panel {
    /// Inline and sticky cards use exactly the same content, colors, width,
    /// padding, selection, and acknowledgement animation. Only placement differs.
    pub(super) fn render_user_prompt(
        &self,
        index: usize,
        text: &str,
        pinned: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let now = Instant::now();
        // Animate position only. Fading the whole card exposes transcript text
        // underneath a sticky prompt during pending/accept acknowledgement.
        let (offset, animating) = self
            .accepted_users
            .get(&index)
            .map(|at| {
                let (offset, _, animating) = crate::ack::motion(*at, now);
                (offset, animating)
            })
            .unwrap_or((0.0, false));
        if animating {
            window.request_animation_frame();
        }
        let number = prompt_number(&self.items, index);
        // Keep the number outside the card in a compact circular badge. Wider
        // turn numbers grow into a pill without taking space from card padding.
        let number_width = number.map_or(0., |number| {
            (number.to_string().len() as f32 * 7. + 8.).max(20.)
        });
        let background = Theme::global()
            .prompt_background(prompt_distance(&self.items, index).unwrap_or(usize::MAX));
        // The model receives `<transcription>` tags. People see a pill instead.
        let (visible, transcribed) = super::voice::tag::strip(text);
        let text: &str = &visible;
        let preview = compact_prompt(text);
        let expanded = self.expanded_prompts.contains(&(index, pinned));
        let collapsed = preview.is_some() && !expanded;
        let card = div()
            .relative()
            .debug_selector(move || {
                if pinned {
                    format!("pinned-user-prompt-{index}")
                } else {
                    format!("user-prompt-{index}")
                }
            })
            .max_w_full()
            .min_w_0()
            .flex_shrink_1()
            .flex()
            .flex_col()
            .px_2()
            .py_1()
            .text_color(Theme::global().TEXT_USER)
            .child(
                div()
                    .id((
                        if pinned {
                            "pinned-prompt-content"
                        } else {
                            "inline-prompt-content"
                        },
                        index,
                    ))
                    .debug_selector(move || {
                        if pinned {
                            format!("pinned-prompt-content-{index}")
                        } else {
                            format!("prompt-content-{index}")
                        }
                    })
                    .when(pinned, |content| {
                        content.max_h(px(180.)).overflow_y_scroll()
                    })
                    // Also bound rendered markdown (headings, tables, images)
                    // and wrapping in narrow chat panes, not just source length.
                    // Clamp wrapped text to whole visual lines with an
                    // ellipsis, so the stepped background is shaped from the
                    // truncated layout instead of being sliced mid-line.
                    .when(collapsed, |content| {
                        // The fill extends 8px/4px past the text. Grow the
                        // clip box by the same amount with padding and cancel
                        // it with negative margins, so layout is unchanged but
                        // the rounded bottom edge is not sliced off.
                        content
                            .line_clamp(COLLAPSED_PROMPT_LINES)
                            .text_ellipsis()
                            .px(px(8.))
                            .mx(px(-8.))
                            .py(px(4.))
                            .my(px(-4.))
                            .max_h(px(COLLAPSED_PROMPT_MAX_HEIGHT + 8.))
                            .overflow_hidden()
                    })
                    .child(markdown::render_prompt_with_prefix(
                        if collapsed { preview.unwrap() } else { text },
                        index,
                        &if pinned {
                            format!("pinned-{index}")
                        } else {
                            index.to_string()
                        },
                        &self.transcript_selection,
                        window,
                        cx,
                        self.media_preview_handler(cx),
                        background,
                    )),
            )
            .when(transcribed, |card| {
                let theme = Theme::global();
                card.child(
                    div()
                        .debug_selector(move || format!("prompt-transcribed-{index}").into())
                        .mt_1()
                        .flex()
                        .items_center()
                        .gap(px(4.))
                        .self_start()
                        .px(px(7.))
                        .h(px(18.))
                        .rounded_full()
                        .bg(theme.INLINE_CODE_BG)
                        .text_size(px(10.))
                        .text_color(theme.TEXT_DIM)
                        .child(
                            gpui::svg()
                                .data(include_bytes!("../../../assets/icons/microphone.svg")
                                    as &'static [u8])
                                .text_color(theme.TEXT_DIM)
                                .size(px(10.)),
                        )
                        .child("Transcribed"),
                )
            })
            .when(preview.is_some(), |card| {
                card.child(
                    div()
                        .id(("prompt-expand", index))
                        .debug_selector(move || {
                            if pinned {
                                format!("pinned-prompt-expand-{index}")
                            } else {
                                format!("prompt-expand-{index}")
                            }
                        })
                        .mt_1()
                        .text_size(px(11.))
                        .text_color(Theme::global().TEXT_DIM)
                        .cursor_pointer()
                        .hover(|style| style.text_color(Theme::global().TEXT_USER))
                        .on_click(cx.listener(move |panel, _, _, cx| {
                            cx.stop_propagation();
                            if !panel.expanded_prompts.remove(&(index, pinned)) {
                                panel.expanded_prompts.insert((index, pinned));
                            }
                            panel.transcript_measurements.dirty = true;
                            cx.notify();
                        }))
                        .child(if expanded {
                            "Collapse prompt"
                        } else {
                            "… Expand prompt"
                        }),
                )
            })
            .into_any_element();
        // Animate and hide the complete row so inline and sticky badges always
        // travel with their cards. Shrink only the card when markdown wraps.
        div()
            .flex()
            .flex_none()
            .w_full()
            .min_w_0()
            .items_start()
            .relative()
            .left(px(offset))
            .when_some(number, |row, number| {
                row.gap(px(6.)).child(
                    div()
                        .debug_selector(move || {
                            if pinned {
                                format!("pinned-prompt-number-{index}-{number}")
                            } else {
                                format!("prompt-number-{index}-{number}")
                            }
                        })
                        .flex_none()
                        .mt(px(4.))
                        .w(px(number_width))
                        .h(px(20.))
                        .rounded_full()
                        .bg(background)
                        .font_family(Theme::global().FONT_MONO)
                        .text_center()
                        .text_size(px(10.))
                        .line_height(px(20.))
                        .text_color(Theme::global().TEXT_DIM)
                        .child(number.to_string()),
                )
            })
            .child(card)
            .into_any_element()
    }

    /// A persistent flex header stays above the transcript. Its presence never
    /// changes on scroll, so historical first rows remain reachable and there
    /// is no scroll-dependent layout boundary or masked transcript content.
    pub(super) fn render_pinned_prompt(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        // Resolve synchronously: the observer describes the previous paint and
        // must never delay a newly submitted prompt or the initial header.
        let index = self.items.iter().rposition(is_pinnable_prompt)?;
        let item = self.items.get(index)?.clone();
        if !is_pinnable_prompt(&item) {
            return None;
        }
        let Item::User(text) = &item else {
            return None;
        };
        let background = Theme::global().panel_background(self.surface_focused);
        Some(
            div()
                .id("pinned-latest-prompt")
                .debug_selector(|| "pinned-latest-prompt".into())
                .flex_none()
                .relative()
                .w_full()
                .max_w(px(1120.))
                .mx_auto()
                .min_w_0()
                // Match the reading surface without overlaying transcript ink.
                .bg(background)
                // Do not cap against last frame's viewport: it is zero before
                // initial layout and can be stale after resize. The compact
                // source preview bounds the normal header independently.
                .max_h(px(240.))
                .overflow_y_scroll()
                .occlude()
                .px_3()
                .text_size(px(13.5))
                .child(self.render_user_prompt(index, text, true, window, cx))
                .into_any_element(),
        )
    }

    /// Read visibility after the list paints. Measuring during render would
    /// still describe the previous scroll position, width, or transcript.
    pub(super) fn prompt_visibility_observer(
        &self,
        prompt_rows: Arc<Vec<(usize, usize)>>,
        first_visible: Rc<Cell<Option<VisibleRows>>>,
        cx: &Context<Self>,
    ) -> gpui::AnyElement {
        let panel = cx.entity().downgrade();
        let following_tail = self.stick_to_bottom;
        let row_count = self.transcript_list.item_count();
        gpui::canvas(
            |_, _, _| (),
            move |_, _, _, cx| {
                let visible = first_visible.take();
                let at_live_end =
                    following_tail || visible.is_some_and(|rows| rows.last + 1 == row_count);
                let offscreen = prompt_for_viewport(&prompt_rows, visible, at_live_end);
                // Historical rows cannot push the persistent latest header off.
                let clip = None;
                cx.defer(move |cx| {
                    // Read first: an update counts as a change for retained views.
                    let Some(entity) = panel.upgrade() else {
                        return;
                    };
                    let current = entity.read(cx);
                    if current.offscreen_prompt == offscreen
                        && current.offscreen_prompt_clip == clip
                    {
                        return;
                    }
                    entity.update(cx, |panel, cx| {
                        panel.offscreen_prompt = offscreen;
                        panel.offscreen_prompt_clip = clip;
                        cx.notify();
                    });
                });
            },
        )
        .absolute()
        .size_full()
        .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_prompt_bounds_lines_and_unicode_without_changing_short_prompts() {
        for text in ["", "Short prompt", "1\n2\n3\n4\n5\n6"] {
            assert_eq!(compact_prompt(text), None);
        }
        assert_eq!(compact_prompt(&"x".repeat(600)), None);
        assert_eq!(
            compact_prompt(&"x".repeat(601)),
            Some("x".repeat(600).as_str())
        );
        let unicode = "🦀日本語".repeat(200);
        let preview = compact_prompt(&unicode).unwrap();
        assert_eq!(preview.chars().count(), 600);
        assert!(unicode.starts_with(preview));
        assert_eq!(
            compact_prompt("1\n2\n3\n4\n5\n6\n7"),
            Some("1\n2\n3\n4\n5\n6")
        );
    }

    #[gpui::test]
    fn long_prompts_expand_inline_and_pinned_independently(cx: &mut gpui::TestAppContext) {
        let (panel, vcx) = cx.add_window_view(|_, cx| {
            Panel::new(
                "long-prompts".into(),
                None,
                None,
                crate::harness::spawn_inert(),
                cx,
            )
        });
        let handle = vcx.update(|window, _| window.window_handle());
        let text = (1..=10)
            .map(|n| format!("Pasted line {n}"))
            .collect::<Vec<_>>()
            .join("\n\n");
        for width in [600., 320.] {
            vcx.simulate_window_resize(handle, gpui::size(px(width), px(800.)));
            panel.update(vcx, |panel, cx| {
                *panel.items = vec![Item::User(text.clone())];
                panel.expanded_prompts.clear();
                panel.stick_to_bottom = false;
                panel.transcript_list.scroll_to(gpui::ListOffset::default());
                cx.notify();
            });
            vcx.run_until_parked();
            let compact = vcx.debug_bounds("pinned-user-prompt-0").unwrap();
            assert!(compact.size.height < px(180.));
            let inline_toggle = vcx.debug_bounds("prompt-expand-0").unwrap();
            vcx.simulate_click(inline_toggle.center(), gpui::Modifiers::default());
            vcx.run_until_parked();
            assert_eq!(vcx.debug_bounds("pinned-user-prompt-0").unwrap(), compact);
            panel.read_with(vcx, |panel, _| {
                assert!(panel.expanded_prompts.contains(&(0, false)));
                assert!(!panel.expanded_prompts.contains(&(0, true)));
            });
            let inline_toggle = vcx.debug_bounds("prompt-expand-0").unwrap();
            vcx.simulate_click(inline_toggle.center(), gpui::Modifiers::default());
            vcx.run_until_parked();
            let toggle = vcx.debug_bounds("pinned-prompt-expand-0").unwrap();
            vcx.simulate_click(toggle.center(), gpui::Modifiers::default());
            vcx.run_until_parked();
            assert!(
                vcx.debug_bounds("pinned-user-prompt-0")
                    .unwrap()
                    .size
                    .height
                    > compact.size.height
            );
            panel.read_with(vcx, |panel, _| {
                assert!(panel.expanded_prompts.contains(&(0, true)));
                assert!(matches!(&panel.items[0], Item::User(original) if original == &text));
            });
            let toggle = vcx.debug_bounds("pinned-prompt-expand-0").unwrap();
            vcx.simulate_click(toggle.center(), gpui::Modifiers::default());
            vcx.run_until_parked();
            assert_eq!(
                vcx.debug_bounds("pinned-user-prompt-0").unwrap().size,
                compact.size
            );
            panel.update(vcx, |panel, cx| {
                panel.expanded_prompts.insert((0, false));
                panel
                    .items
                    .extend((0..40).map(|n| Item::Assistant(format!("Response {n}"))));
                panel.stick_to_bottom = true;
                cx.notify();
            });
            vcx.run_until_parked();
            assert!(vcx.debug_bounds("pinned-latest-prompt").is_some());
            assert_eq!(
                vcx.debug_bounds("pinned-user-prompt-0").unwrap().size,
                compact.size
            );
            let toggle = vcx.debug_bounds("pinned-prompt-expand-0").unwrap();
            vcx.simulate_click(toggle.center(), gpui::Modifiers::default());
            vcx.run_until_parked();
            assert!(
                vcx.debug_bounds("pinned-user-prompt-0")
                    .unwrap()
                    .size
                    .height
                    > compact.size.height
            );
            panel.read_with(vcx, |panel, _| {
                assert!(panel.expanded_prompts.contains(&(0, true)))
            });
            let toggle = vcx.debug_bounds("pinned-prompt-expand-0").unwrap();
            vcx.simulate_click(toggle.center(), gpui::Modifiers::default());
            vcx.run_until_parked();
            assert_eq!(
                vcx.debug_bounds("pinned-user-prompt-0").unwrap().size,
                compact.size
            );
        }
    }

    #[gpui::test]
    fn collapsed_wrapped_prompt_clamps_to_whole_lines(cx: &mut gpui::TestAppContext) {
        let (panel, vcx) = cx.add_window_view(|_, cx| {
            Panel::new(
                "clamped-prompt".into(),
                None,
                None,
                crate::harness::spawn_inert(),
                cx,
            )
        });
        let handle = vcx.update(|window, _| window.window_handle());
        let text = "Wrapped words fill many visual lines here. ".repeat(40);
        for width in [900., 600., 320.] {
            vcx.simulate_window_resize(handle, gpui::size(px(width), px(800.)));
            panel.update(vcx, |panel, cx| {
                *panel.items = vec![Item::User(text.clone())];
                panel.expanded_prompts.clear();
                cx.notify();
            });
            vcx.run_until_parked();
            // The clip box adds 4px of fill padding above and below the text.
            let content = vcx.debug_bounds("pinned-prompt-content-0").unwrap();
            let line_height = 13.5 * 1.55;
            let lines = (f32::from(content.size.height) - 8.) / line_height;
            assert!(
                (lines - lines.round()).abs() < 0.05,
                "collapsed prompt at {width}px shows a partial line: {lines}"
            );
            assert!(lines.round() as usize <= COLLAPSED_PROMPT_LINES);
            // The clip box is cancelled by margins, so the card still hugs text.
            let card = vcx.debug_bounds("pinned-user-prompt-0").unwrap();
            assert!(card.left() <= content.left() + px(0.5));
        }
    }

    const BACKGROUND_NOTICES: [&str; 4] = [
        "**Background task started** `task-42` · `Workspace tests`",
        "**Background task progress** `task-42` · `Workspace tests` (`bash`)\n\n35% · Running tests",
        "**Background task** `task-42` · `Workspace tests` (`bash`) · ✓ completed · 8.2s · exit 0",
        "**Background task stalled** `task-42` · `Workspace tests` (`bash`) · no output or progress for 30s (running 60s total)",
    ];

    #[test]
    fn prompt_numbers_ignore_notices_and_remain_stable_on_append() {
        let mut items = vec![Item::User("First".into()), Item::Assistant("Answer".into())];
        for notice in BACKGROUND_NOTICES {
            items.push(Item::User(notice.into()));
            assert_eq!(prompt_number(&items, items.len() - 1), None);
        }
        items.push(Item::User("  ".into()));
        assert_eq!(prompt_number(&items, items.len() - 1), None);
        assert_eq!(prompt_number(&items, 1), None);
        assert_eq!(prompt_number(&items, items.len()), None);
        for number in 2..=15 {
            items.push(Item::User(format!("Prompt {number}")));
            assert_eq!(prompt_number(&items, items.len() - 1), Some(number));
            assert_eq!(prompt_number(&items, 0), Some(1));
        }
    }

    fn assert_number_geometry(vcx: &mut gpui::VisualTestContext, card: gpui::Bounds<gpui::Pixels>) {
        let number = vcx
            .debug_bounds("pinned-prompt-number-0-1")
            .expect("genuine prompt is numbered");
        assert_eq!(
            card.left() - number.right(),
            px(6.),
            "badge sits left of card"
        );
        assert_eq!(number.top(), card.top() + px(4.));
        assert_eq!(number.size, gpui::size(px(20.), px(20.)));
        assert!(number.left() >= vcx.debug_bounds("transcript").unwrap().left());
        let content = vcx.debug_bounds("pinned-prompt-content-0").unwrap();
        assert!(
            number.right() < content.left(),
            "number never overlaps prompt content"
        );
        assert_eq!(
            card.size.height,
            content.size.height + px(8.),
            "number adds no footer height"
        );
        assert_eq!(
            card.size.width,
            content.size.width + px(16.),
            "number does not change card padding"
        );
    }

    #[test]
    fn background_notifications_are_never_pinnable_prompts() {
        for notice in BACKGROUND_NOTICES {
            assert!(!is_pinnable_prompt(&Item::User(format!("\n{notice}\n"))));
        }
        assert!(!is_pinnable_prompt(&Item::User("  ".into())));
        assert!(is_pinnable_prompt(&Item::User(
            "Explain background tasks".into()
        )));
        assert!(!is_pinnable_prompt(&Item::BackgroundTask {
            task_id: "task-42".into(),
            label: "Workspace tests".into(),
            summary: "Running tests".into(),
            percent: Some(35.),
            done: false,
        }));
    }

    #[test]
    fn only_real_user_turns_age_prompt_card_backgrounds() {
        let mut items = vec![Item::User("First prompt".into())];
        assert_eq!(prompt_distance(&items, 0), Some(0));
        items.push(Item::Assistant("Response".into()));
        items.push(Item::Tool {
            call_id: "call".into(),
            name: "bash".into(),
            input: "{}".into(),
            output: "Done".into(),
            done: true,
            error: None,
        });
        for notice in BACKGROUND_NOTICES {
            items.push(Item::User(notice.into()));
            assert_eq!(prompt_distance(&items, items.len() - 1), None);
        }
        items.push(Item::BackgroundTask {
            task_id: "task".into(),
            label: "Tests".into(),
            summary: "Running".into(),
            percent: None,
            done: false,
        });
        assert_eq!(prompt_distance(&items, 0), Some(0));
        items.push(Item::User("Second prompt".into()));
        assert_eq!(prompt_distance(&items, 0), Some(1));
        assert_eq!(prompt_distance(&items, items.len() - 1), Some(0));
        items.push(Item::User("Third prompt".into()));
        assert_eq!(prompt_distance(&items, 0), Some(2));
        assert_eq!(prompt_distance(&items, items.len() - 1), Some(0));
        assert_eq!(prompt_distance(&items, items.len()), None);
    }

    #[gpui::test]
    fn background_notifications_stay_in_the_scroller(cx: &mut gpui::TestAppContext) {
        let (panel, vcx) = cx.add_window_view(|_, cx| {
            Panel::new(
                "background-no-pin".into(),
                None,
                None,
                crate::harness::spawn_inert(),
                cx,
            )
        });
        let handle = vcx.update(|window, _| window.window_handle());
        vcx.simulate_window_resize(handle, gpui::size(px(600.), px(400.)));
        for notice in BACKGROUND_NOTICES {
            for real_prompt in [false, true] {
                panel.update(vcx, |panel, cx| {
                    panel.items.clear();
                    if real_prompt {
                        panel.items.push(Item::User("Run the tests".into()));
                    }
                    panel.items.push(Item::User(notice.into()));
                    for i in 0..40 {
                        panel
                            .items
                            .push(Item::Assistant(format!("Response paragraph {i}")));
                    }
                    cx.notify();
                });
                vcx.run_until_parked();
                panel.read_with(vcx, |panel, _| {
                    assert_eq!(panel.offscreen_prompt, real_prompt.then_some(0));
                    assert!(
                        panel.transcript_render_rows().iter().any(|row| {
                            matches!(row.source, TranscriptRowSource::Settled(index)
                            if matches!(&panel.items[index], Item::User(text) if text == notice))
                        }),
                        "background output remains in transcript history"
                    );
                });
                assert_eq!(
                    vcx.debug_bounds("pinned-latest-prompt").is_some(),
                    real_prompt
                );
            }
        }
    }

    fn visible(first: usize, last: usize, first_top: f32) -> Option<VisibleRows> {
        Some(VisibleRows {
            first,
            last,
            first_top: px(first_top),
            next_prompt_top: None,
        })
    }

    #[test]
    fn latest_prompt_survives_initial_paint_middle_attachment_and_scroll() {
        let prompts = [(0, 0), (12, 16), (30, 40)];
        assert_eq!(prompt_for_viewport(&[], None, false), None);
        for live in [false, true] {
            for geometry in [
                None,
                visible(0, 5, 0.),
                visible(12, 30, -5.),
                visible(30, 35, 0.),
                visible(30, 35, -10.),
                visible(31, 31, -1800.),
                visible(35, 40, -5.),
            ] {
                assert_eq!(prompt_for_viewport(&prompts, geometry, live), Some(40));
            }
        }
        assert_eq!(
            prompt_for_viewport(&[(0, 0), (12, 16)], visible(3, 12, -5.), true),
            Some(16)
        );
    }

    #[gpui::test]
    fn sticky_card_keeps_original_geometry_at_partial_scroll_threshold(
        cx: &mut gpui::TestAppContext,
    ) {
        let (panel, vcx) = cx.add_window_view(|_, cx| {
            Panel::new(
                "sticky-geometry".into(),
                None,
                None,
                crate::harness::spawn_inert(),
                cx,
            )
        });
        let handle = vcx.update(|window, _| window.window_handle());
        for width in [600., 320.] {
            vcx.simulate_window_resize(handle, gpui::size(px(width), px(800.)));
            for text in [
                "Hi".to_string(),
                "A **formatted** prompt that wraps naturally. ".repeat(12),
            ] {
                panel.update(vcx, |panel, cx| {
                    *panel.items = vec![Item::User(text.clone())];
                    for n in 0..40 {
                        panel
                            .items
                            .push(Item::Assistant(format!("Response paragraph {n}")));
                    }
                    panel.stick_to_bottom = false;
                    panel.transcript_list.scroll_to(gpui::ListOffset::default());
                    cx.notify();
                });
                vcx.run_until_parked();
                panel.update(vcx, |panel, cx| {
                    panel.stick_to_bottom = false;
                    panel.transcript_list.scroll_to(gpui::ListOffset::default());
                    cx.notify();
                });
                vcx.run_until_parked();
                let original = vcx.debug_bounds("pinned-user-prompt-0").unwrap();
                assert_number_geometry(vcx, original);
                let viewport = vcx.debug_bounds("transcript").unwrap();
                assert_eq!(original.bottom(), viewport.top());
                let inline = vcx.debug_bounds("user-prompt-0").unwrap();
                assert!(
                    inline.top() >= viewport.top(),
                    "first historical row is not masked by the header"
                );
                assert!(
                    inline.size.height > px(0.),
                    "original row is visible, not a blank spacer"
                );
                // Cross in both directions, including an exactly aligned card,
                // fractional pixel offsets, and a still mostly visible row.
                for offset in [9.5, 10., 10.5, 15., 10., 9.5, 0.] {
                    panel.update(vcx, |panel, cx| {
                        panel.transcript_list.scroll_to(gpui::ListOffset {
                            item_ix: 0,
                            offset_in_item: px(offset),
                        });
                        cx.notify();
                    });
                    vcx.run_until_parked();
                    let card = vcx
                        .debug_bounds("pinned-user-prompt-0")
                        .expect("card never disappears");
                    assert_number_geometry(vcx, card);
                    assert_eq!(
                        card.size, original.size,
                        "sticky markdown has identical size at width {width}, offset {offset}"
                    );
                    assert_eq!(card.left(), original.left(), "no horizontal jump");
                    assert_eq!(
                        card.top(),
                        original.top(),
                        "latest card stays above transcript"
                    );
                    assert_eq!(vcx.debug_bounds("transcript").unwrap(), viewport);
                    assert_eq!(vcx.debug_bounds("pinned-latest-prompt").is_some(), true);
                    if let Some(pinned) = vcx.debug_bounds("pinned-latest-prompt") {
                        assert_eq!(pinned.top(), card.top(), "no banner padding above the card");
                        assert_eq!(
                            pinned.size.height, card.size.height,
                            "sticky wrapper must neither clip the card to a banner nor add bottom padding"
                        );
                    }
                    assert!(
                        vcx.debug_bounds("transcript-row-0").is_some(),
                        "threshold must be reached before the prompt row disappears"
                    );
                }
            }
        }
    }

    #[gpui::test]
    fn prompt_cards_fit_content_without_captions(cx: &mut gpui::TestAppContext) {
        let (panel, vcx) = cx.add_window_view(|_, cx| {
            Panel::new(
                "compact-prompts".into(),
                None,
                None,
                crate::harness::spawn_inert(),
                cx,
            )
        });
        let handle = vcx.update(|window, _| window.window_handle());
        vcx.simulate_window_resize(handle, gpui::size(px(600.), px(800.)));
        for width in [600., 320.] {
            vcx.simulate_window_resize(handle, gpui::size(px(width), px(800.)));
            for text in [
                "Hi".to_string(),
                "A longer prompt with **formatted text** that should wrap naturally. ".repeat(5),
            ] {
                panel.update(vcx, |panel, cx| {
                    *panel.items = vec![Item::User(text.clone())];
                    cx.notify();
                });
                vcx.run_until_parked();
                let card = vcx
                    .debug_bounds("pinned-user-prompt-0")
                    .expect("prompt paints");
                assert_number_geometry(vcx, card);
                let viewport = vcx.debug_bounds("transcript").unwrap();
                assert!(card.left() >= viewport.left());
                assert!(
                    card.right() <= viewport.right(),
                    "prompt stays within chat width"
                );
                assert!(vcx.debug_bounds("role-caption-you, 1").is_none());
                if text == "Hi" {
                    assert!(
                        card.size.width < px(100.),
                        "short prompt hugs its content: {card:?}"
                    );
                } else {
                    assert!(card.size.height > px(50.), "long prompt wraps");
                }
            }
        }
    }

    #[gpui::test]
    fn multi_digit_prompt_numbers_stay_in_left_badges(cx: &mut gpui::TestAppContext) {
        let (panel, vcx) = cx.add_window_view(|_, cx| {
            Panel::new(
                "pinned-prompt-number-gutter".into(),
                None,
                None,
                crate::harness::spawn_inert(),
                cx,
            )
        });
        let handle = vcx.update(|window, _| window.window_handle());
        for width in [600., 320.] {
            vcx.simulate_window_resize(handle, gpui::size(px(width), px(800.)));
            for text in [
                "Hi".to_string(),
                "A **formatted** prompt with wrapping content. ".repeat(8),
            ] {
                for (count, card_selector, content_selector, number_selector) in [
                    (
                        99,
                        "pinned-user-prompt-98",
                        "pinned-prompt-content-98",
                        "pinned-prompt-number-98-99",
                    ),
                    (
                        100,
                        "pinned-user-prompt-99",
                        "pinned-prompt-content-99",
                        "pinned-prompt-number-99-100",
                    ),
                ] {
                    panel.update(vcx, |panel, cx| {
                        *panel.items = (1..count).map(|_| Item::User("Earlier".into())).collect();
                        panel.items.push(Item::User(text.clone()));
                        panel.stick_to_bottom = true;
                        panel.transcript_list.scroll_to_end();
                        cx.notify();
                    });
                    vcx.run_until_parked();
                    let card = vcx.debug_bounds(card_selector).unwrap();
                    let content = vcx.debug_bounds(content_selector).unwrap();
                    let number = vcx.debug_bounds(number_selector).unwrap();
                    assert!(
                        number.right() < content.left(),
                        "multi-digit number never overlaps markdown"
                    );
                    assert_eq!(number.right() + px(6.), card.left());
                    assert_eq!(number.top(), card.top() + px(4.));
                    assert_eq!(number.size.width, px(if count == 99 { 22. } else { 29. }));
                    assert!(number.left() >= vcx.debug_bounds("transcript").unwrap().left());
                    assert_eq!(card.size.width, content.size.width + px(16.));
                    assert_eq!(
                        card.size.height,
                        content.size.height + px(8.),
                        "no numbered footer"
                    );
                    assert!(card.right() <= vcx.debug_bounds("transcript").unwrap().right());
                }
            }
        }
    }

    #[gpui::test]
    fn initial_header_and_tall_attachment_never_leave_a_gap(cx: &mut gpui::TestAppContext) {
        let mut bytes = std::io::Cursor::new(Vec::new());
        image::RgbaImage::from_pixel(64, 2048, image::Rgba([240, 20, 60, 255]))
            .write_to(&mut bytes, image::ImageFormat::Png)
            .unwrap();
        let image = TranscriptImage::from_rendered(jcode_sdk::RenderedImage {
            history_message_index: None,
            media_type: "image/png".into(),
            data: base64::engine::general_purpose::STANDARD.encode(bytes.into_inner()),
            label: Some("Tall attachment".into()),
            source: jcode_sdk::RenderedImageSource::UserInput,
            anchor: None,
        });
        let (panel, vcx) = cx.add_window_view(|window, cx| {
            let mut panel = Panel::new(
                "attachment-header".into(),
                None,
                None,
                crate::harness::spawn_inert(),
                cx,
            );
            *panel.items = vec![
                Item::User("Earlier prompt".into()),
                Item::Image(image),
                Item::User("Latest prompt below the attachment".into()),
                Item::Assistant("Answer".into()),
            ];
            assert_eq!(panel.offscreen_prompt, None);
            assert_eq!(panel.transcript_list.viewport_bounds().size.height, px(0.));
            assert!(
                panel.render_pinned_prompt(window, cx).is_some(),
                "initial render needs no visibility observation"
            );
            panel
        });
        let handle = vcx.update(|window, _| window.window_handle());
        vcx.simulate_window_resize(handle, gpui::size(px(600.), px(500.)));
        for row in [0, 1, 2, 0] {
            panel.update(vcx, |panel, cx| {
                panel.stick_to_bottom = false;
                panel.transcript_list.scroll_to(gpui::ListOffset {
                    item_ix: row,
                    offset_in_item: px(0.),
                });
                // Reproduce stale observer data that previously clipped the
                // complete header when another prompt entered the viewport.
                panel.offscreen_prompt_clip = Some(px(0.));
                cx.notify();
            });
            vcx.run_until_parked();
            let header = vcx.debug_bounds("pinned-latest-prompt").unwrap();
            assert_eq!(
                header.bottom(),
                vcx.debug_bounds("transcript").unwrap().top()
            );
            assert!(header.size.height > px(0.) && header.size.height < px(100.));
            let card = vcx.debug_bounds("pinned-user-prompt-2").unwrap();
            assert_eq!(card.top(), header.top());
            assert!(card.bottom() <= header.bottom());
            panel.read_with(vcx, |panel, _| assert_eq!(panel.offscreen_prompt, Some(2)));
        }
        panel.update(vcx, |panel, cx| {
            panel.items.push(Item::User("Just submitted".into()));
            panel.offscreen_prompt = Some(0);
            cx.notify();
        });
        vcx.run_until_parked();
        assert_eq!(
            vcx.debug_bounds("pinned-user-prompt-4").unwrap().top(),
            vcx.debug_bounds("pinned-latest-prompt").unwrap().top()
        );
    }

    #[gpui::test]
    fn latest_header_survives_native_scroll_and_new_prompt(cx: &mut gpui::TestAppContext) {
        let (panel, vcx) = cx.add_window_view(|_, cx| {
            Panel::new(
                "persistent-latest".into(),
                None,
                None,
                crate::harness::spawn_inert(),
                cx,
            )
        });
        let handle = vcx.update(|window, _| window.window_handle());
        vcx.simulate_window_resize(handle, gpui::size(px(600.), px(500.)));
        panel.update(vcx, |panel, cx| {
            for turn in 0..3 {
                panel.items.push(Item::User(format!("Prompt {turn}")));
                for n in 0..40 {
                    panel.items.push(Item::Assistant(format!("Response {n}")));
                }
            }
            cx.notify();
        });
        vcx.run_until_parked();
        let viewport = vcx.debug_bounds("transcript").unwrap();
        let header = vcx.debug_bounds("pinned-latest-prompt").unwrap();
        assert!(header.size.height > px(0.));
        for delta in [180., 2400., 2400., -180., -2400.] {
            vcx.simulate_event(gpui::ScrollWheelEvent {
                position: viewport.center(),
                delta: gpui::ScrollDelta::Pixels(gpui::point(px(0.), px(delta))),
                modifiers: Default::default(),
                touch_phase: gpui::TouchPhase::Moved,
            });
            vcx.run_until_parked();
            scroll_momentum_tests::settle(vcx);
            assert_eq!(vcx.debug_bounds("pinned-latest-prompt").unwrap(), header);
            assert_eq!(
                vcx.debug_bounds("pinned-user-prompt-82").unwrap().top(),
                header.top()
            );
            assert_eq!(vcx.debug_bounds("transcript").unwrap(), viewport);
            panel.read_with(vcx, |panel, _| {
                assert_eq!(panel.offscreen_prompt, Some(82));
                assert_eq!(panel.offscreen_prompt_clip, None);
            });
        }
        panel.update(vcx, |panel, cx| {
            panel.stick_to_bottom = false;
            panel.items.push(Item::User("Newest prompt".into()));
            cx.notify();
        });
        vcx.run_until_parked();
        assert_eq!(
            vcx.debug_bounds("pinned-user-prompt-123").unwrap().top(),
            header.top()
        );
        panel.read_with(vcx, |panel, _| {
            assert_eq!(panel.offscreen_prompt, Some(123))
        });
    }

    #[gpui::test]
    fn scrolling_history_keeps_latest_turn(cx: &mut gpui::TestAppContext) {
        let (panel, vcx) = cx.add_window_view(|_, cx| {
            Panel::new(
                "historical-prompts".into(),
                None,
                None,
                crate::harness::spawn_inert(),
                cx,
            )
        });
        panel.update(vcx, |panel, cx| {
            for turn in 0..3 {
                panel.items.push(Item::User(format!("Prompt {turn}")));
                for line in 0..40 {
                    panel
                        .items
                        .push(Item::Assistant(format!("Turn {turn} response {line}")));
                }
            }
            cx.notify();
        });
        vcx.run_until_parked();
        panel.update(vcx, |panel, _| assert_eq!(panel.offscreen_prompt, Some(82)));
        for (row, expected) in [(50, Some(82)), (10, Some(82)), (0, Some(82))] {
            panel.update(vcx, |panel, cx| {
                panel.stick_to_bottom = false;
                panel.transcript_list.scroll_to(gpui::ListOffset {
                    item_ix: row,
                    offset_in_item: px(0.),
                });
                cx.notify();
            });
            vcx.run_until_parked();
            panel.update(vcx, |panel, _| assert_eq!(panel.offscreen_prompt, expected));
        }
    }

    #[gpui::test]
    fn pinned_prompt_transition_does_not_move_the_transcript(cx: &mut gpui::TestAppContext) {
        let (panel, vcx) = cx.add_window_view(|_, cx| {
            Panel::new(
                "prompt-transition".into(),
                None,
                None,
                crate::harness::spawn_inert(),
                cx,
            )
        });
        let handle = vcx.update(|window, _| window.window_handle());
        vcx.simulate_window_resize(handle, gpui::size(px(600.), px(500.)));
        panel.update(vcx, |panel, cx| {
            panel.items.push(Item::User("First prompt".into()));
            for n in 0..40 {
                panel
                    .items
                    .push(Item::Assistant(format!("Response paragraph {n}")));
            }
            cx.notify();
        });
        vcx.run_until_parked();
        panel.update(vcx, |panel, cx| {
            panel.stick_to_bottom = false;
            panel.transcript_list.scroll_to(gpui::ListOffset::default());
            cx.notify();
        });
        vcx.run_until_parked();
        let viewport = vcx.debug_bounds("transcript").unwrap();
        assert!(vcx.debug_bounds("pinned-latest-prompt").is_some());

        for (delta, expected_pin) in [(-4., true), (4., true)] {
            let mut crossed = false;
            for _ in 0..80 {
                let before = vcx.debug_bounds("transcript-row-1").unwrap().top();
                vcx.simulate_event(gpui::ScrollWheelEvent {
                    position: viewport.center(),
                    delta: gpui::ScrollDelta::Pixels(gpui::point(px(0.), px(delta))),
                    modifiers: Default::default(),
                    touch_phase: gpui::TouchPhase::Moved,
                });
                vcx.run_until_parked();
                scroll_momentum_tests::settle(vcx);
                assert_eq!(
                    vcx.debug_bounds("transcript").unwrap(),
                    viewport,
                    "pinning must not resize or move the scroll viewport"
                );
                let after = vcx.debug_bounds("transcript-row-1").unwrap().top();
                assert!(
                    (f32::from(after - before) - delta).abs() < 0.5,
                    "text must move only by the scroll delta, not the header height"
                );
                if vcx.debug_bounds("pinned-latest-prompt").is_some() == expected_pin {
                    crossed = true;
                    break;
                }
            }
            assert!(crossed, "native scrolling must cross the pin boundary");
            for _ in 0..4 {
                panel.update(vcx, |_, cx| cx.notify());
                vcx.run_until_parked();
                assert_eq!(vcx.debug_bounds("transcript").unwrap(), viewport);
                assert_eq!(
                    vcx.debug_bounds("pinned-latest-prompt").is_some(),
                    expected_pin
                );
            }
        }
    }

    #[gpui::test]
    fn pinned_prompt_tracks_scrolling_and_new_prompts(cx: &mut gpui::TestAppContext) {
        let (panel, vcx) = cx.add_window_view(|_, cx| {
            Panel::new(
                "prompt-visibility".into(),
                None,
                None,
                crate::harness::spawn_inert(),
                cx,
            )
        });
        panel.update(vcx, |panel, cx| {
            *panel.items = vec![Item::User("Keep the normal **prompt card**".into())];
            cx.notify();
        });
        vcx.run_until_parked();
        assert!(vcx.debug_bounds("pinned-latest-prompt").is_some());

        // With no todos, a long response still pins the offscreen prompt.
        panel.update(vcx, |panel, cx| {
            for i in 0..40 {
                panel
                    .items
                    .push(Item::Assistant(format!("Response paragraph {i}")));
            }
            cx.notify();
        });
        vcx.run_until_parked();
        assert!(vcx.debug_bounds("pinned-latest-prompt").is_some());

        // A partially scrolled original remains sticky once its card top crosses.
        panel.update(vcx, |panel, cx| {
            panel.stick_to_bottom = false;
            panel.transcript_list.scroll_to(gpui::ListOffset {
                item_ix: 0,
                offset_in_item: px(15.),
            });
            cx.notify();
        });
        vcx.run_until_parked();
        assert!(vcx.debug_bounds("transcript-row-0").is_some());
        assert!(vcx.debug_bounds("pinned-latest-prompt").is_some());

        panel.update(vcx, |panel, cx| {
            panel.stick_to_bottom = true;
            cx.notify();
        });
        vcx.run_until_parked();
        assert!(vcx.debug_bounds("pinned-latest-prompt").is_some());

        panel.update(vcx, |panel, cx| {
            panel.items.push(Item::User("A new visible prompt".into()));
            cx.notify();
        });
        vcx.run_until_parked();
        assert!(vcx.debug_bounds("pinned-latest-prompt").is_some());
    }

    #[gpui::test]
    fn pinned_prompt_card_is_below_todos_and_resize_aware(cx: &mut gpui::TestAppContext) {
        let (panel, vcx) = cx.add_window_view(|_, cx| {
            Panel::new(
                "prompt-todos".into(),
                None,
                None,
                crate::harness::spawn_inert(),
                cx,
            )
        });
        let handle = vcx.update(|window, _| window.window_handle());
        vcx.simulate_window_resize(handle, gpui::size(px(600.), px(400.)));
        panel.update(vcx, |panel, cx| {
            *panel.items = vec![
                Item::User("Keep the prompt below the tasks".into()),
                Item::Todos(serde_json::from_value(serde_json::json!({
                    "todos": [{"id":"one", "content":"Verify visibility", "status":"in_progress", "priority":"high"}]
                })).unwrap()),
            ];
            for i in 0..20 {
                panel.items.push(Item::Assistant(format!("Response paragraph {i}")));
            }
            cx.notify();
        });
        vcx.run_until_parked();
        let todos = vcx.debug_bounds("pinned-todo-card").unwrap();
        let prompt = vcx.debug_bounds("pinned-latest-prompt").unwrap();
        let transcript = vcx.debug_bounds("transcript").unwrap();
        assert_eq!(
            prompt.top() - todos.bottom(),
            px(8.),
            "todo breathing room belongs above the whole viewport"
        );
        assert_eq!(prompt.bottom(), transcript.top());
        assert!(prompt.bottom() < transcript.bottom());
        assert!(
            vcx.debug_bounds("pinned-prompt-fade").is_none(),
            "no overlay masks transcript content"
        );

        // Workspace selection, not keyboard focus, chooses the reading surface.
        // Switching it must invalidate cached rendering without moving the card.
        for focused in [false, true] {
            panel.update(vcx, |panel, cx| panel.set_surface_focused(focused, cx));
            vcx.run_until_parked();
            panel.update(vcx, |panel, _| assert_eq!(panel.surface_focused, focused));
            assert_eq!(vcx.debug_bounds("pinned-latest-prompt").unwrap(), prompt);
            assert!(vcx.debug_bounds("pinned-prompt-fade").is_none());
        }

        panel.update(vcx, |_, cx| cx.notify());
        vcx.run_until_parked();
        let todos = vcx.debug_bounds("pinned-todo-card").unwrap();
        let prompt = vcx.debug_bounds("pinned-latest-prompt").unwrap();
        assert_eq!(
            prompt.top() - todos.bottom(),
            px(8.),
            "expanded todo has the same gap"
        );
        assert_eq!(
            prompt.bottom(),
            vcx.debug_bounds("transcript").unwrap().top()
        );

        vcx.simulate_window_resize(handle, gpui::size(px(600.), px(1600.)));
        vcx.run_until_parked();
        assert!(vcx.debug_bounds("pinned-latest-prompt").is_some());
        assert!(vcx.debug_bounds("pinned-todo-card").is_some());
        assert!(vcx.debug_bounds("pinned-prompt-fade").is_none());
        panel.update(vcx, |panel, cx| {
            panel.items.retain(|item| !matches!(item, Item::Todos(_)));
            cx.notify();
        });
        vcx.simulate_window_resize(handle, gpui::size(px(600.), px(400.)));
        vcx.run_until_parked();
        assert!(vcx.debug_bounds("pinned-todo-card").is_none());
        let prompt = vcx.debug_bounds("pinned-latest-prompt").unwrap();
        assert_eq!(prompt.top(), px(8.), "no-todo prompt keeps a top gap");
        assert_eq!(
            prompt.bottom(),
            vcx.debug_bounds("transcript").unwrap().top()
        );
    }

    #[gpui::test]
    fn pinned_prompt_aligns_with_centered_transcript_column(cx: &mut gpui::TestAppContext) {
        let (panel, vcx) = cx.add_window_view(|_, cx| {
            Panel::new(
                "pinned-prompt-align".into(),
                None,
                None,
                crate::harness::spawn_inert(),
                cx,
            )
        });
        let handle = vcx.update(|window, _| window.window_handle());
        vcx.simulate_window_resize(handle, gpui::size(px(1600.), px(800.)));
        panel.update(vcx, |panel, cx| {
            *panel.items = vec![
                Item::User("Check the centered alignment on a wide window.".into()),
                Item::Assistant("Aligned with the 1120px transcript column.".into()),
            ];
            cx.notify();
        });
        vcx.run_until_parked();

        let prompt = vcx
            .debug_bounds("pinned-latest-prompt")
            .expect("pinned prompt should render");
        let row = vcx
            .debug_bounds("transcript-row-0")
            .expect("first transcript row should render");

        assert_eq!(
            prompt.size.width,
            px(1120.),
            "pinned prompt should cap at the 1120px chat column width"
        );
        assert_eq!(
            prompt.left(),
            row.left(),
            "pinned prompt left edge should align with the transcript column"
        );
        assert_eq!(
            prompt.right(),
            row.right(),
            "pinned prompt right edge should align with the transcript column"
        );
    }
}
