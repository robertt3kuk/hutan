//! UI pieces every GPUI crate of Hutan shares. `hutan-desktop`, `hutan-code-view` and
//! `hutan-terminal-view` cannot depend on each other, so a piece two of them need lives here:
//! today the scrollbar ([`scrollbar`]).

pub mod scrollbar;

pub use scrollbar::{
    Axis, Mark, Metrics, Policy, Region, ScrollHandleSource, Scrollbar, Scroller, Style, Visibility,
};
