use gpui::{Bounds, Pixels, Size, StyledText, div, hsla, point, prelude::*, px};

use crate::ui_client::config::TRANSCRIPT_FADE_EDGE;
use crate::ui_client::model::UiState;

pub fn render_transcript_layout(state: &UiState) -> gpui::Div {
    let text = state.transcript.text.as_str();

    let fade_edge = if text.trim().chars().count() >= 50 {
        TRANSCRIPT_FADE_EDGE
    } else {
        0.0
    };

    div().size_full().flex().flex_col().child(
        div()
            .id("transcript-scroll")
            .w_full()
            .flex_1()
            .px(px(4.0))
            .pt(px(4.0))
            .mb(px(10.0))
            .pb(px(10.0))
            .track_scroll(&state.transcript.scroll)
            .overflow_y_scroll()
            .overflow_fade_y(px(fade_edge))
            .child({
                let bold_from = state.transcript.bold_from.min(text.len());

                div()
                    .w_full()
                    .whitespace_normal()
                    .text_sm()
                    .line_height(px(20.0))
                    .text_color(hsla(0.0, 0.0, 0.97, 0.85))
                    .child(if text.trim().is_empty() {
                        div()
                            .text_color(hsla(0.0, 0.0, 0.97, 0.45))
                            .child("Start speaking...")
                            .into_any_element()
                    } else if bold_from > 0 && bold_from < text.len() {
                        StyledText::new(text.to_string())
                            .with_highlights([(
                                bold_from..text.len(),
                                gpui::FontWeight::SEMIBOLD.into(),
                            )])
                            .into_any_element()
                    } else {
                        StyledText::new(text.to_string()).into_any_element()
                    })
            }),
    )
}

pub fn bottom_center_bounds(
    visible: Bounds<Pixels>,
    window_size: Size<Pixels>,
    safe_bottom_margin: f32,
) -> Bounds<Pixels> {
    let safe_bottom = px(safe_bottom_margin);

    let origin = point(
        visible.center().x - window_size.center().x,
        visible.origin.y + visible.size.height - window_size.height - safe_bottom,
    );

    Bounds {
        origin,
        size: window_size,
    }
}
