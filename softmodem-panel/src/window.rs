// SPDX-FileCopyrightText: 2026 Wiktor Bryk <contact@itsvic.dev>
//
// SPDX-License-Identifier: GPL-3.0-or-later

//! The front panel in a window of its own.

use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{
    App, Bounds, BoxShadow, Context, Div, FontWeight, Hsla, Image, ImageFormat, Render,
    RenderImage, TitlebarOptions, Window, WindowBounds, WindowOptions, div, hsla, img,
    linear_color_stop, linear_gradient, prelude::*, px, size,
};
use tokio::sync::watch;
use tracing::warn;

use crate::lights::Lights;
use crate::{Status, text};

const WIDTH: f32 = 880.0;
const HEIGHT: f32 = 360.0;
const FRAME: Duration = Duration::from_millis(16);
const FIRST_LIGHT: f32 = 93.0;
const LIGHT_SPACING: f32 = 46.0;
const LIGHT_Y: f32 = 207.0;
const LIGHT_SIZE: (f32, f32) = (24.0, 7.0);
const TEXT_Y: f32 = 304.0;

/// Shows the panel for `status` in a window titled `title`, until the window
/// closes or `done` completes. It takes the thread over, which on macOS must
/// be the main thread.
pub fn run(
    title: String,
    status: watch::Receiver<Status>,
    done: impl Future<Output = ()> + 'static,
) {
    gpui_platform::application().run(move |cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(WIDTH), px(HEIGHT)), cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(TitlebarOptions {
                title: Some(title.into()),
                ..TitlebarOptions::default()
            }),
            is_resizable: false,
            ..WindowOptions::default()
        };
        if cx
            .open_window(options, |_, cx| cx.new(|cx| Panel::new(status, cx)))
            .is_err()
        {
            cx.quit();
            return;
        }
        cx.on_window_closed(|cx, _| cx.quit()).detach();
        cx.spawn(async move |cx| {
            done.await;
            cx.update(|cx| cx.quit());
        })
        .detach();
        cx.activate(true);
    });
}

struct Panel {
    status: watch::Receiver<Status>,
    lights: Lights,
    summary: Vec<String>,
    case: Option<Arc<RenderImage>>,
}

impl Panel {
    fn new(status: watch::Receiver<Status>, cx: &mut Context<Self>) -> Self {
        let now = Instant::now();
        let shown = status.borrow().clone();
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(FRAME).await;
                if this.update(cx, Panel::tick).is_err() {
                    break;
                }
            }
        })
        .detach();
        // Drawn now, as an image gpui loads by itself shows only at the next redraw.
        let case = Image::from_bytes(
            ImageFormat::Svg,
            include_bytes!("../assets/case.svg").to_vec(),
        )
        .to_image_data(cx.svg_renderer())
        .inspect_err(|error| warn!(%error, "could not draw the case"))
        .ok();
        Self {
            status,
            lights: Lights::new(&shown, now),
            summary: text::summary(&shown, now),
            case,
        }
    }

    fn tick(&mut self, cx: &mut Context<Self>) {
        let now = Instant::now();
        let status = self.status.borrow().clone();
        let summary = text::summary(&status, now);
        let changed = self.lights.update(&status, now) || summary != self.summary;
        self.summary = summary;
        if changed {
            cx.notify();
        }
    }
}

impl Render for Panel {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let lights = (0_u8..)
            .zip(self.lights.levels())
            .map(|(i, level)| light(FIRST_LIGHT + LIGHT_SPACING * f32::from(i), level));
        div()
            .relative()
            .size_full()
            .bg(hsla(0.0, 0.0, 0.05, 1.0))
            .children(
                self.case
                    .clone()
                    .map(|case| img(case).absolute().w(px(WIDTH)).h(px(HEIGHT))),
            )
            .children(lights)
            .child(summary(&self.summary))
    }
}

fn light(centre: f32, level: f32) -> Div {
    let (width, height) = LIGHT_SIZE;
    let off = hsla(0.0, 0.75, 0.1, 1.0);
    let on = hsla(0.008, 1.0, 0.5, 1.0);
    div()
        .absolute()
        .left(px(centre - width / 2.0))
        .top(px(LIGHT_Y - height / 2.0))
        .w(px(width))
        .h(px(height))
        .rounded(px(1.5))
        .bg(mix(off, on, level))
        .shadow(vec![
            glow(hsla(0.0, 1.0, 0.42, 0.55 * level), 26.0, 5.0),
            glow(hsla(0.005, 1.0, 0.5, 0.85 * level), 8.0, 1.0),
            BoxShadow::new(px(0.0), px(0.0), hsla(0.05, 1.0, 0.82, 0.95 * level))
                .blur_radius(px(3.0))
                .spread_radius(px(-1.0))
                .inset(),
            BoxShadow::new(px(0.0), px(1.0), hsla(0.0, 0.0, 0.0, 0.7 * (1.0 - level)))
                .blur_radius(px(1.5))
                .inset(),
        ])
}

fn glow(color: Hsla, blur: f32, spread: f32) -> BoxShadow {
    BoxShadow::new(px(0.0), px(0.0), color)
        .blur_radius(px(blur))
        .spread_radius(px(spread))
}

fn mix(from: Hsla, to: Hsla, amount: f32) -> Hsla {
    let between = |a: f32, b: f32| a + (b - a) * amount;
    hsla(
        between(from.h, to.h),
        between(from.s, to.s),
        between(from.l, to.l),
        between(from.a, to.a),
    )
}

// A strip of embossing tape, its letters pressed up from behind.
fn summary(items: &[String]) -> Div {
    let words = items.join("  ·  ").to_uppercase();
    let (across, down) = (14.0, 5.0);
    let tape = div()
        .relative()
        .px(px(across))
        .py(px(down))
        .rounded(px(1.5))
        .bg(linear_gradient(
            180.0,
            linear_color_stop(hsla(0.0, 0.0, 0.16, 1.0), 0.0),
            linear_color_stop(hsla(0.0, 0.0, 0.05, 1.0), 0.6),
        ))
        .shadow(vec![
            BoxShadow::new(px(0.0), px(2.0), hsla(0.0, 0.0, 0.0, 0.65)).blur_radius(px(4.0)),
            BoxShadow::new(px(0.0), px(1.0), hsla(0.0, 0.0, 1.0, 0.12)).inset(),
        ])
        .font_family("Menlo")
        .font_weight(FontWeight::BOLD)
        .text_size(px(11.0))
        .child(
            div()
                .absolute()
                .left(px(across))
                .top(px(down + 1.0))
                .text_color(hsla(0.0, 0.0, 0.0, 0.9))
                .child(words.clone()),
        )
        .child(
            div()
                .relative()
                .text_color(hsla(0.0, 0.0, 0.9, 1.0))
                .child(words),
        );
    div()
        .absolute()
        .left(px(0.0))
        .top(px(TEXT_Y))
        .w(px(WIDTH))
        .flex()
        .justify_center()
        .child(tape)
}
