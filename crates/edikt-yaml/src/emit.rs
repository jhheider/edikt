//! Emit a [`Value`] as a fresh YAML document, for conversion targets (`-T yaml`).
//!
//! This is the *data-model* path — trivia is already gone by the time we get a
//! `Value`, so there is nothing to preserve. We drive libyaml-safer's emitter
//! (block style), which gets scalar quoting right for free — the reason we lean
//! on its emitter here rather than hand-roll one.

use edikt_core::{EditError, Value};
use libyaml_safer::{Emitter, Encoding, Event, MappingStyle, ScalarStyle, SequenceStyle};

use crate::scalar::{format_float, needs_quoting};

/// Emit `value` as YAML. Returns the text and (always empty — YAML holds every
/// feature) the lossy-conversion warnings, matching the other emitters' shape.
pub fn emit(value: &Value) -> Result<(String, Vec<String>), EditError> {
    let mut out: Vec<u8> = Vec::new();
    {
        let mut emitter = Emitter::new();
        emitter.set_output_string(&mut out);
        emitter.set_unicode(true);
        emit_event(&mut emitter, Event::stream_start(Encoding::Utf8))?;
        emit_event(&mut emitter, Event::document_start(None, &[], true))?;
        emit_value(&mut emitter, value)?;
        emit_event(&mut emitter, Event::document_end(true))?;
        emit_event(&mut emitter, Event::stream_end())?;
        emitter.flush().map_err(err)?;
    }
    let text = String::from_utf8(out).map_err(err)?;
    Ok((text, Vec::new()))
}

fn emit_value(emitter: &mut Emitter, value: &Value) -> Result<(), EditError> {
    match value {
        Value::Array(items) => {
            emit_event(
                emitter,
                Event::sequence_start(None, None, true, SequenceStyle::Block),
            )?;
            for item in items {
                emit_value(emitter, item)?;
            }
            emit_event(emitter, Event::sequence_end())?;
        }
        Value::Object(entries) => {
            emit_event(
                emitter,
                Event::mapping_start(None, None, true, MappingStyle::Block),
            )?;
            for (k, v) in entries {
                // Keys are strings; quote where a plain key would be misread.
                emit_scalar_value(emitter, &Value::Str(k.clone()))?;
                emit_value(emitter, v)?;
            }
            emit_event(emitter, Event::mapping_end())?;
        }
        other => emit_scalar_value(emitter, other)?,
    }
    Ok(())
}

/// Emit a scalar `Value`, choosing style and implicit flags so it round-trips its
/// type: numbers/bools/null go plain; a string that would be misread is forced to
/// double-quoted. libyaml then handles the actual quoting/escaping.
fn emit_scalar_value(emitter: &mut Emitter, value: &Value) -> Result<(), EditError> {
    let (text, style, plain_implicit) = match value {
        Value::Null => ("null".to_string(), ScalarStyle::Plain, true),
        Value::Bool(b) => (b.to_string(), ScalarStyle::Plain, true),
        Value::Int(i) => (i.to_string(), ScalarStyle::Plain, true),
        Value::Float(f) => (format_float(*f), ScalarStyle::Plain, true),
        Value::Str(s) if needs_quoting(s) => (s.clone(), ScalarStyle::DoubleQuoted, false),
        Value::Str(s) => (s.clone(), ScalarStyle::Plain, true),
        Value::Array(_) | Value::Object(_) => {
            return Err(EditError::new(
                "internal: collection reached scalar emitter",
            ));
        }
    };
    emit_event(
        emitter,
        Event::scalar(None, None, &text, plain_implicit, true, style),
    )
}

fn emit_event(emitter: &mut Emitter, event: Event) -> Result<(), EditError> {
    emitter.emit(event).map_err(err)
}

fn err(e: impl std::fmt::Display) -> EditError {
    EditError::new(e.to_string())
}
