//! The small set of primitives the whole UI is built from: flat bordered
//! buttons, tonal chips, a 3px progress track and monospace figures.

use gpui::prelude::*;
use gpui::{div, px, rgb, Div, IntoElement, SharedString};

use crate::theme;

pub const BTN_H: f32 = 28.0;

/// Neutral bordered button. The caller attaches the click handler.
pub fn button(label: impl Into<SharedString>) -> Div {
    div()
        .flex()
        .items_center()
        .justify_center()
        .h(px(BTN_H))
        .px(px(10.))
        .rounded(px(3.))
        .bg(rgb(theme::palette().elevated))
        .border_1()
        .border_color(rgb(theme::palette().border))
        .text_size(px(theme::TEXT_SM))
        .text_color(rgb(theme::palette().text))
        .hover(|style| {
            style
                .bg(rgb(theme::palette().hover))
                .border_color(rgb(theme::palette().border_strong))
                .cursor_pointer()
        })
        .child(label.into())
}

/// The one high-emphasis action on a screen.
pub fn primary_button(label: impl Into<SharedString>) -> Div {
    div()
        .flex()
        .items_center()
        .justify_center()
        .h(px(BTN_H))
        .px(px(12.))
        .rounded(px(3.))
        .bg(rgb(theme::palette().accent))
        .text_size(px(theme::TEXT_SM))
        .text_color(rgb(theme::palette().on_accent))
        .hover(|style| {
            style
                .bg(rgb(theme::palette().accent_hover))
                .cursor_pointer()
        })
        .child(label.into())
}

/// Borderless button for secondary actions.
pub fn ghost_button(label: impl Into<SharedString>) -> Div {
    div()
        .flex()
        .items_center()
        .justify_center()
        .h(px(BTN_H))
        .px(px(8.))
        .rounded(px(3.))
        .text_size(px(theme::TEXT_SM))
        .text_color(rgb(theme::palette().text_dim))
        .hover(|style| {
            style
                .bg(rgb(theme::palette().hover))
                .text_color(rgb(theme::palette().text))
                .cursor_pointer()
        })
        .child(label.into())
}

pub fn danger_button(label: impl Into<SharedString>) -> Div {
    div()
        .flex()
        .items_center()
        .justify_center()
        .h(px(BTN_H))
        .px(px(10.))
        .rounded(px(3.))
        .border_1()
        .border_color(rgb(theme::palette().danger_soft))
        .text_size(px(theme::TEXT_SM))
        .text_color(rgb(theme::palette().danger))
        .hover(|style| {
            style
                .bg(rgb(theme::palette().danger_soft))
                .border_color(rgb(theme::palette().danger))
                .cursor_pointer()
        })
        .child(label.into())
}

/// Compact tonal label.
pub fn chip(label: impl Into<SharedString>, fg: u32, bg: u32) -> Div {
    div()
        .flex()
        .items_center()
        .h(px(20.))
        .px(px(7.))
        .rounded(px(3.))
        .bg(rgb(bg))
        .text_size(px(11.))
        .text_color(rgb(fg))
        .child(label.into())
}

pub fn dot(color: u32) -> Div {
    div()
        .size(px(6.))
        .rounded_full()
        .bg(rgb(color))
        .flex_shrink_0()
}

/// Segmented control: one option highlighted, the rest quiet.
pub fn segment(label: impl Into<SharedString>, selected: bool) -> Div {
    let base = div()
        .flex()
        .items_center()
        .justify_center()
        .h(px(24.))
        .px(px(9.))
        .rounded(px(3.))
        .text_size(px(theme::TEXT_SM));
    if selected {
        base.bg(rgb(theme::palette().accent_soft))
            .text_color(rgb(theme::palette().accent_hover))
            .border_1()
            .border_color(rgb(theme::palette().accent_dim))
            .child(label.into())
    } else {
        base.text_color(rgb(theme::palette().text_dim))
            .border_1()
            .border_color(rgb(theme::palette().border))
            .hover(|style| {
                style
                    .bg(rgb(theme::palette().hover))
                    .text_color(rgb(theme::palette().text))
                    .cursor_pointer()
            })
            .child(label.into())
    }
}

/// Compact segment for groups that must fit a narrow column.
pub fn segment_small(label: impl Into<SharedString>, selected: bool) -> Div {
    let base = div()
        .flex()
        .items_center()
        .justify_center()
        .h(px(22.))
        .px(px(5.))
        .rounded(px(3.))
        .text_size(px(11.));
    if selected {
        base.bg(rgb(theme::palette().accent_soft))
            .text_color(rgb(theme::palette().accent_hover))
            .border_1()
            .border_color(rgb(theme::palette().accent_dim))
            .child(label.into())
    } else {
        base.text_color(rgb(theme::palette().text_dim))
            .border_1()
            .border_color(rgb(theme::palette().border))
            .hover(|style| {
                style
                    .bg(rgb(theme::palette().hover))
                    .text_color(rgb(theme::palette().text))
                    .cursor_pointer()
            })
            .child(label.into())
    }
}

/// 3px track with a filled fraction; `fraction` is 0..=1.
pub fn progress(fraction: f32, color: u32) -> Div {
    div()
        .h(px(3.))
        .w_full()
        .rounded_full()
        .bg(rgb(theme::palette().track))
        .overflow_hidden()
        .child(
            div()
                .h(px(3.))
                .w(gpui::relative(fraction.clamp(0.0, 1.0)))
                .bg(rgb(color))
                .rounded_full(),
        )
}

pub fn faint(text: impl Into<SharedString>) -> Div {
    div()
        .text_size(px(theme::TEXT_SM))
        .text_color(rgb(theme::palette().text_faint))
        .child(text.into())
}

/// Figures line up because every one of them is monospace.
pub fn mono(text: impl Into<SharedString>, color: u32) -> Div {
    div()
        .font_family(theme::FONT_MONO)
        .text_size(px(theme::TEXT_SM))
        .text_color(rgb(color))
        .child(text.into())
}

pub fn keycap(text: impl Into<SharedString>) -> Div {
    div()
        .flex()
        .items_center()
        .justify_center()
        .h(px(18.))
        .px(px(5.))
        .rounded(px(2.))
        .bg(rgb(theme::palette().elevated))
        .border_1()
        .border_color(rgb(theme::palette().border_strong))
        .font_family(theme::FONT_MONO)
        .text_size(px(11.))
        .text_color(rgb(theme::palette().text_dim))
        .child(text.into())
}

/// Uppercase, wide-tracked section heading.
pub fn section(text: impl Into<SharedString>) -> Div {
    div()
        .text_size(px(11.))
        .text_color(rgb(theme::palette().text_faint))
        .child(text.into())
}

pub fn divider() -> Div {
    div().h(px(1.)).w_full().bg(rgb(theme::palette().border))
}

pub fn spacer() -> Div {
    div().flex_1()
}

/// A settings row: label on the left, control on the right.
pub fn setting_row(
    label_text: impl Into<SharedString>,
    hint: Option<&'static str>,
    control: impl IntoElement,
) -> Div {
    let mut text = div().flex().flex_col().gap(px(2.)).flex_1().child(
        div()
            .text_size(px(theme::TEXT_BASE))
            .text_color(rgb(theme::palette().text))
            .child(label_text.into()),
    );
    if let Some(hint) = hint {
        text = text.child(faint(hint));
    }
    div()
        .flex()
        .flex_row()
        .items_center()
        .justify_between()
        .gap(px(12.))
        .py(px(7.))
        .child(text)
        .child(div().flex().items_center().gap(px(6.)).child(control))
}

/// Centered call to action for an empty pane.
pub fn empty_state(title: impl Into<SharedString>, lines: Vec<&'static str>) -> Div {
    let mut body = div().flex().flex_col().items_center().gap(px(6.)).child(
        div()
            .text_size(px(theme::TEXT_LG))
            .text_color(rgb(theme::palette().text_dim))
            .child(title.into()),
    );
    for line in lines {
        body = body.child(faint(line));
    }
    div()
        .flex()
        .flex_col()
        .flex_1()
        .items_center()
        .justify_center()
        .gap(px(10.))
        .child(body)
}
