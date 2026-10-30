//! The one agent truth, GPUI-free (ADR 0025, `docs/architecture.md`): how the daemon's sessions,
//! requests and observation trees become one record per agent and the groups a person reads
//! them in (*Requests*, *Failed*, *Ready for review*, *Working*, …).
//!
//! The desktop's Agents page, panel, sidebar, Today rail and badges read these records; the
//! daemon links the same reducer to serve a board, so a model never sees another rule set than
//! the window. Pure functions of protocol types: no I/O, no clock, no GPUI, no `client`.
//!
//! - [`presentation`]: the reducer ([`presentation::build`]) and its records
//!   ([`presentation::AgentRecord`], [`presentation::Group`], [`presentation::State`]).
//! - [`attention`]: what wants the user (unread marks and what a session says of itself in
//!   lists, tabs and the macOS surfaces) and the tracker that remembers what was not seen yet.
//! - [`agents`]: the observation tree: which nodes are listed, durations, usage labels, badge.
//! - [`models`]: the display names of the harnesses' models.
//! - [`titles`]: a title without the pictographs an agent put in front of it.
//!
//! What a state looks like (icons, colors, layout) is the window's: this crate only says which
//! state, which [`attention::Tone`] and which words.

pub mod agents;
pub mod attention;
pub mod models;
pub mod presentation;
pub mod titles;
