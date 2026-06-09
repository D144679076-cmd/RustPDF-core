//! PDF content stream interpretation.
//!
//! - [`graphics_state`] — graphics state machine (CTM, colors, line style)
//! - [`text_state`] — text positioning and font state
//! - [`operators`] — content stream tokenization into operator+operand pairs
//! - [`operator`] — typed operator vocabulary for exhaustive dispatch
//! - [`interpreter`] — main dispatch loop and OutputDevice trait

pub mod graphics_state;
pub mod interpreter;
pub mod operator;
pub mod operators;
pub mod text_state;
