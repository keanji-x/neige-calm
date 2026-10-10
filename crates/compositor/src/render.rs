//! Per-window software rendering: one offscreen pixman image and one damage
//! tracker per window. A frame holds the window's surface tree and its popups,
//! with the window geometry at the origin, and nothing else.

use std::sync::Arc;

use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::damage::OutputDamageTracker;
use smithay::backend::renderer::element::AsRenderElements;
use smithay::backend::renderer::element::surface::WaylandSurfaceRenderElement;
use smithay::backend::renderer::pixman::PixmanRenderer;
use smithay::backend::renderer::{Bind, ExportMem, Offscreen};
use smithay::reexports::pixman::Image;
use smithay::utils::{Physical, Point, Rectangle, Scale, Transform};

use crate::api::{Error, Frame, Rect, Result};
use crate::watch::full;
use crate::windows::Tracked;

const CLEAR: [f32; 4] = [0.0, 0.0, 0.0, 1.0];

pub(crate) struct Canvas {
    image: Image<'static, 'static>,
    tracker: OutputDamageTracker,
    size: (u32, u32),
    /// 0 until the image holds a complete render; the image is reused, so 1 after.
    age: usize,
}

impl Canvas {
    fn new(renderer: &mut PixmanRenderer, size: (u32, u32)) -> std::result::Result<Self, String> {
        let image = renderer
            .create_buffer(Fourcc::Xrgb8888, (size.0 as i32, size.1 as i32).into())
            .map_err(|e| e.to_string())?;
        Ok(Self {
            image,
            tracker: OutputDamageTracker::new(
                (size.0 as i32, size.1 as i32),
                1.0,
                Transform::Normal,
            ),
            size,
            age: 0,
        })
    }
}

/// Renders the window and publishes the result to its watchers when anything
/// changed. Returns the full current frame with the render's damage.
pub(crate) fn render(renderer: &mut PixmanRenderer, tracked: &mut Tracked) -> Result<Frame> {
    let id = tracked.id;
    if !tracked.has_buffer() {
        return Err(Error::NoBuffer(id));
    }
    let geometry = tracked.window.geometry();
    if geometry.size.w <= 0 || geometry.size.h <= 0 {
        return Err(Error::NoBuffer(id));
    }
    let size = (geometry.size.w as u32, geometry.size.h as u32);
    let failed = |e: String| Error::Render(id, e);
    if tracked.canvas.as_ref().is_none_or(|c| c.size != size) {
        tracked.canvas = Some(Canvas::new(renderer, size).map_err(failed)?);
    }
    let Some(canvas) = tracked.canvas.as_mut() else {
        return Err(Error::Render(id, "canvas missing".into()));
    };

    let origin: Point<i32, Physical> = Point::from((-geometry.loc.x, -geometry.loc.y));
    let elements: Vec<WaylandSurfaceRenderElement<PixmanRenderer>> = tracked
        .window
        .render_elements(renderer, origin, Scale::from(1.0), 1.0);

    let mut target = renderer
        .bind(&mut canvas.image)
        .map_err(|e| failed(e.to_string()))?;
    let result = canvas
        .tracker
        .render_output(renderer, &mut target, canvas.age, &elements, CLEAR)
        .map_err(|e| failed(format!("{e:?}")))?;
    canvas.age = 1;
    let damage: Vec<Rect> = result
        .damage
        .map(|rects| rects.iter().filter_map(|r| clamp(r, size)).collect())
        .unwrap_or_default();

    let region = Rectangle::from_size((size.0 as i32, size.1 as i32).into());
    let mapping = renderer
        .copy_framebuffer(&target, region, Fourcc::Xrgb8888)
        .map_err(|e| failed(e.to_string()))?;
    let pixels = renderer
        .map_texture(&mapping)
        .map_err(|e| failed(e.to_string()))?;
    let stride = size.0 * 4;
    let frame = Frame {
        size,
        stride,
        xrgb8888: Arc::from(&pixels[..(stride * size.1) as usize]),
        damage,
    };
    tracked.dirty = false;

    if !frame.damage.is_empty() && tracked.is_watched() {
        for watcher in tracked.watchers.iter().filter_map(|w| w.upgrade()) {
            watcher.publish(frame.clone());
        }
    }
    Ok(frame)
}

/// The same frame with its damage set to the whole frame.
pub(crate) fn whole(frame: Frame) -> Frame {
    Frame {
        damage: vec![full(frame.size)],
        ..frame
    }
}

fn clamp(rect: &Rectangle<i32, Physical>, size: (u32, u32)) -> Option<Rect> {
    let x0 = rect.loc.x.clamp(0, size.0 as i32);
    let y0 = rect.loc.y.clamp(0, size.1 as i32);
    let x1 = (rect.loc.x + rect.size.w).clamp(0, size.0 as i32);
    let y1 = (rect.loc.y + rect.size.h).clamp(0, size.1 as i32);
    (x1 > x0 && y1 > y0).then(|| Rect {
        x: x0 as u32,
        y: y0 as u32,
        width: (x1 - x0) as u32,
        height: (y1 - y0) as u32,
    })
}
