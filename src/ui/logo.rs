//! The Tabula mark (design.md, Brand · Tabula logo): a table whose header
//! row and middle column light up to form a T. Drawn from shapes so it's
//! sharp at any size, in a dark and a light variant. The app icon is the
//! same drawing as SVG (`packaging/icons/tabula.svg`).

use gpui::{
    BoxShadow, Div, Hsla, ParentElement, Pixels, Styled, div, hsla, linear_color_stop,
    linear_gradient, point, px, rgb, rgba,
};

/// The geometry is drawn on a 280-unit tile, as in the design.
const UNIT: f32 = 280.;

/// The lit cells' gradient ends, dark and light variants.
fn lit(light: bool) -> (Hsla, Hsla) {
    if light {
        (rgb(0x7c8cff).into(), rgb(0x4a3bd6).into())
    } else {
        (rgb(0xb4bdff).into(), rgb(0x6a55f0).into())
    }
}

/// The mark at `size` (its width and height). `light` is the white tile
/// for light backgrounds.
pub fn logo(size: Pixels, light: bool) -> Div {
    let scale = f32::from(size) / UNIT;
    let at = |units: f32| px(units * scale);
    let (lit_from, lit_to) = lit(light);
    let (tile_from, tile_to): (Hsla, Hsla) = if light {
        (rgb(0xffffff).into(), rgb(0xeef0f7).into())
    } else {
        (rgb(0x262a36).into(), rgb(0x0b0c0f).into())
    };
    // Dim cells: white on the dark tile, indigo on the light one.
    let (cell_fill, cell_edge): (Hsla, Hsla) = if light {
        (rgba(0x4f5fe80f).into(), rgba(0x4f5fe824).into())
    } else {
        (rgba(0xffffff0d).into(), rgba(0xffffff17).into())
    };
    let glow: Hsla = rgb(0x7c8cff).into();
    let cell = |left: f32, top: f32| {
        div()
            .absolute()
            .left(at(left))
            .top(at(top))
            .size(at(56.875))
            .rounded(at(13.125))
            .bg(cell_fill)
            .border(at(1.37).max(px(0.5)))
            .border_color(cell_edge)
    };
    let lit_shape = |left: f32, top: f32, width: f32, height: f32, angle: f32| {
        div()
            .absolute()
            .left(at(left))
            .top(at(top))
            .w(at(width))
            .h(at(height))
            .rounded(at(13.125))
            .bg(linear_gradient(
                angle,
                linear_color_stop(lit_from, 0.),
                linear_color_stop(lit_to, 1.),
            ))
            .border(at(1.37).max(px(0.5)))
            .border_color(hsla(0., 0., 1., 0.28))
    };
    // The header's two column lines.
    let divider = |left: f32| {
        div()
            .absolute()
            .left(at(left))
            .top(at(62.34))
            .w(at(1.64).max(px(0.5)))
            .h(at(24.06))
            .rounded(at(0.82))
            .bg(hsla(0., 0., 1., 0.35))
    };
    // A soft glow behind the T: rings fading out (gpui has no radial
    // gradient or blur).
    let glow_ring = |radius: f32, alpha: f32| {
        div()
            .absolute()
            .left(at(140. - radius))
            .top(at(125.78 - radius))
            .size(at(radius * 2.))
            .rounded_full()
            .bg(glow.opacity(alpha))
    };
    let glow_strength = if light { 0.6 } else { 1. };
    div()
        .relative()
        .flex_none()
        .size(size)
        .rounded(at(62.89))
        .overflow_hidden()
        .bg(linear_gradient(
            155.,
            linear_color_stop(tile_from, 0.),
            linear_color_stop(tile_to, 1.),
        ))
        .border(at(1.64).max(px(0.5)))
        .border_color(if light {
            hsla(0., 0., 0., 0.08)
        } else {
            hsla(0., 0., 1., 0.08)
        })
        .shadow(vec![BoxShadow {
            color: hsla(0., 0., 0., if light { 0.14 } else { 0.45 }),
            offset: point(px(0.), at(13.125)),
            blur_radius: at(26.25),
            spread_radius: px(0.),
            inset: false,
        }])
        .child(glow_ring(114.8, 0.05 * glow_strength))
        .child(glow_ring(80., 0.06 * glow_strength))
        .child(glow_ring(48., 0.07 * glow_strength))
        .child(cell(45.94, 111.56))
        .child(cell(177.19, 111.56))
        .child(cell(45.94, 177.19))
        .child(cell(177.19, 177.19))
        .child(lit_shape(111.56, 111.56, 56.875, 122.5, 150.))
        .child(lit_shape(45.94, 45.94, 188.125, 56.875, 135.))
        .child(divider(106.37))
        .child(divider(171.99))
}
