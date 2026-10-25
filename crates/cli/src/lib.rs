//! Building blocks of the `hutan` CLI that the integration slice reuses for `attach`:
//! [`render::HostRenderer`] draws the daemon's render stream into the host terminal,
//! [`detach::DetachDetector`] filters the detach key out of host input, and [`host`] wraps raw
//! mode and size. [`chat_render::ChatPrinter`] prints agent chat events as plain text, and
//! [`proxy`] is the bytes-only bridge of `hutan proxy`.

pub mod chat_render;
pub mod detach;
pub mod host;
pub mod proxy;
pub mod render;

pub use detach::DetachDetector;
pub use render::HostRenderer;
