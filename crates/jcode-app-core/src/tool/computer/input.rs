//! Synthetic mouse + keyboard input via Core Graphics CGEvents.
//!
//! This is the *visible* control path: events go to the shared HID stream, so
//! they move the real cursor and type into the focused app. Background control
//! lives in `ax.rs` instead.

use super::keys;
use anyhow::{Context, Result, bail};
use core_graphics::event::{
    CGEvent, CGEventFlags, CGEventTapLocation, CGEventType, CGMouseButton, EventField,
    ScrollEventUnit,
};
use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
use core_graphics::geometry::CGPoint;
use std::thread::sleep;
use std::time::Duration;

#[derive(Clone, Copy)]
pub enum Button {
    Left,
    Right,
}

fn source() -> Result<CGEventSource> {
    CGEventSource::new(CGEventSourceStateID::HIDSystemState).map_err(|_| {
        anyhow::anyhow!(
            "failed to create CGEventSource. Grant Accessibility permission (run the `setup` action)."
        )
    })
}

fn post(event: CGEvent) {
    event.post(CGEventTapLocation::HID);
}

/// Current cursor position in global (top-left origin) screen points.
pub fn current_cursor() -> Result<CGPoint> {
    let src = source()?;
    let evt = CGEvent::new(src).map_err(|_| anyhow::anyhow!("failed to read cursor position"))?;
    Ok(evt.location())
}

pub fn move_to(x: f64, y: f64) -> Result<()> {
    let src = source()?;
    let evt = CGEvent::new_mouse_event(
        src,
        CGEventType::MouseMoved,
        CGPoint::new(x, y),
        CGMouseButton::Left,
    )
    .map_err(|_| anyhow::anyhow!("failed to create mouse-move event"))?;
    post(evt);
    Ok(())
}

pub fn click(x: Option<f64>, y: Option<f64>, button: Button, count: u32) -> Result<CGPoint> {
    let point = match (x, y) {
        (Some(x), Some(y)) => CGPoint::new(x, y),
        _ => current_cursor()?,
    };
    let (down, up, cg_button) = match button {
        Button::Left => (
            CGEventType::LeftMouseDown,
            CGEventType::LeftMouseUp,
            CGMouseButton::Left,
        ),
        Button::Right => (
            CGEventType::RightMouseDown,
            CGEventType::RightMouseUp,
            CGMouseButton::Right,
        ),
    };

    let src = source()?;
    let mv = CGEvent::new_mouse_event(src, CGEventType::MouseMoved, point, cg_button)
        .map_err(|_| anyhow::anyhow!("failed to create move event"))?;
    post(mv);
    sleep(Duration::from_millis(10));

    for i in 1..=count {
        let src_d = source()?;
        let down_evt = CGEvent::new_mouse_event(src_d, down, point, cg_button)
            .map_err(|_| anyhow::anyhow!("failed to create mouse-down event"))?;
        if count > 1 {
            down_evt.set_integer_value_field(EventField::MOUSE_EVENT_CLICK_STATE, i as i64);
        }
        post(down_evt);

        let src_u = source()?;
        let up_evt = CGEvent::new_mouse_event(src_u, up, point, cg_button)
            .map_err(|_| anyhow::anyhow!("failed to create mouse-up event"))?;
        if count > 1 {
            up_evt.set_integer_value_field(EventField::MOUSE_EVENT_CLICK_STATE, i as i64);
        }
        post(up_evt);
        sleep(Duration::from_millis(20));
    }
    Ok(point)
}

pub fn drag(from_x: f64, from_y: f64, to_x: f64, to_y: f64) -> Result<()> {
    let from = CGPoint::new(from_x, from_y);
    let to = CGPoint::new(to_x, to_y);

    let src = source()?;
    let down = CGEvent::new_mouse_event(src, CGEventType::LeftMouseDown, from, CGMouseButton::Left)
        .map_err(|_| anyhow::anyhow!("failed to create drag-down event"))?;
    post(down);
    sleep(Duration::from_millis(30));

    let steps = 10;
    for i in 1..=steps {
        let t = i as f64 / steps as f64;
        let p = CGPoint::new(from_x + (to_x - from_x) * t, from_y + (to_y - from_y) * t);
        let src_m = source()?;
        let mv =
            CGEvent::new_mouse_event(src_m, CGEventType::LeftMouseDragged, p, CGMouseButton::Left)
                .map_err(|_| anyhow::anyhow!("failed to create drag-move event"))?;
        post(mv);
        sleep(Duration::from_millis(15));
    }

    let src_u = source()?;
    let up = CGEvent::new_mouse_event(src_u, CGEventType::LeftMouseUp, to, CGMouseButton::Left)
        .map_err(|_| anyhow::anyhow!("failed to create drag-up event"))?;
    post(up);
    Ok(())
}

pub fn scroll(x: Option<f64>, y: Option<f64>, dx: i32, dy: i32) -> Result<()> {
    if let (Some(x), Some(y)) = (x, y) {
        move_to(x, y)?;
        sleep(Duration::from_millis(10));
    }
    let src = source()?;
    let evt = CGEvent::new_scroll_event(src, ScrollEventUnit::PIXEL, 2, dy, dx, 0)
        .map_err(|_| anyhow::anyhow!("failed to create scroll event"))?;
    post(evt);
    Ok(())
}

/// Type a UTF-8 string as a single synthesized keyboard event (Unicode payload),
/// layout-independent. Goes to the focused app.
pub fn type_text(text: &str) -> Result<()> {
    let src = source()?;
    let down = CGEvent::new_keyboard_event(src, 0, true)
        .map_err(|_| anyhow::anyhow!("failed to create keyboard event"))?;
    down.set_string(text);
    post(down);

    let src_up = source()?;
    let up = CGEvent::new_keyboard_event(src_up, 0, false)
        .map_err(|_| anyhow::anyhow!("failed to create keyboard event"))?;
    up.set_string(text);
    post(up);
    Ok(())
}

/// Parse a chord like "cmd+shift+t" into (modifier flags, main keycode).
pub fn parse_chord(chord: &str) -> Result<(CGEventFlags, u16)> {
    let mut flags = CGEventFlags::CGEventFlagNull;
    let mut keycode: Option<u16> = None;
    for raw in chord.split('+') {
        let part = raw.trim().to_lowercase();
        if part.is_empty() {
            continue;
        }
        match part.as_str() {
            "cmd" | "command" | "meta" | "super" => flags |= CGEventFlags::CGEventFlagCommand,
            "ctrl" | "control" => flags |= CGEventFlags::CGEventFlagControl,
            "alt" | "opt" | "option" => flags |= CGEventFlags::CGEventFlagAlternate,
            "shift" => flags |= CGEventFlags::CGEventFlagShift,
            "fn" => flags |= CGEventFlags::CGEventFlagSecondaryFn,
            other => {
                if keycode.is_some() {
                    bail!("key chord '{chord}' has more than one non-modifier key");
                }
                keycode = Some(
                    keys::keycode_for(other)
                        .with_context(|| format!("unknown key '{other}' in chord '{chord}'"))?,
                );
            }
        }
    }
    let code = keycode.with_context(|| format!("chord '{chord}' has no main key"))?;
    Ok((flags, code))
}

pub fn key_chord(chord: &str) -> Result<()> {
    let (flags, code) = parse_chord(chord)?;
    let src = source()?;
    let down = CGEvent::new_keyboard_event(src, code, true)
        .map_err(|_| anyhow::anyhow!("failed to create key-down event"))?;
    down.set_flags(flags);
    post(down);
    sleep(Duration::from_millis(15));

    let src_up = source()?;
    let up = CGEvent::new_keyboard_event(src_up, code, false)
        .map_err(|_| anyhow::anyhow!("failed to create key-up event"))?;
    up.set_flags(flags);
    post(up);
    Ok(())
}

pub fn key_hold(chord: &str, down_state: bool) -> Result<()> {
    let (flags, code) = parse_chord(chord)?;
    let src = source()?;
    let evt = CGEvent::new_keyboard_event(src, code, down_state)
        .map_err(|_| anyhow::anyhow!("failed to create key event"))?;
    evt.set_flags(flags);
    post(evt);
    Ok(())
}
