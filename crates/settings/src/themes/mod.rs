//! Color packs (ADR 0018): data-only TOML themes for the chrome, the terminal, state and diff
//! roles, fonts, density and effects. `hutan-dark` and `hutan-light` are embedded packs parsed
//! by the same loader as a user's `<data dir>/themes/<id>/theme.toml`. No GPUI here (ADR 0010):
//! the desktop turns a [`ResolvedTheme`] into GPUI colors when it applies a snapshot.
//!
//! - [`tokens`]: the exhaustive token registry (ids, docs, bounds, fallbacks).
//! - [`manifest`]: parse and validate `theme.toml` (schema v1).
//! - [`resolve`]: base + pack + user overrides -> one immutable [`ResolvedTheme`].
//! - [`catalog`]: built-ins plus discovered packs as a snapshot, selection by polarity.
//! - [`registry`]: the watcher, debounce, generations and last-good snapshots.
//! - [`contrast`]: warning-only WCAG checks.

mod catalog;
mod color;
mod confine;
mod contrast;
pub mod icons;
mod manifest;
mod registry;
mod resolve;
mod svg;
mod tokens;

pub use catalog::{
    DisabledPack, MAX_CANDIDATES, MAX_DIAGNOSTICS, MAX_TOTAL_MANIFEST_BYTES, OverrideMemory,
    PackSource, Selected, SelectionRequest, ThemeCatalogSnapshot, ThemeDescriptor, ThemePack,
    builtin_for, builtin_ids, builtin_theme, load_pack,
};
pub use color::{ParseRgbError, Rgb, Rgba};
pub use contrast::{ContrastIssue, contrast_issues, contrast_ratio};
pub use manifest::{
    Builtin, Declaration, MAX_DEPTH, MAX_ENTRIES, MAX_FAMILIES, MAX_MANIFEST_BYTES, Meta, Parsed,
    SCHEMA_VERSION, parse,
};
pub use registry::{ThemeRegistry, ThemeWatchGuard};
pub use resolve::{
    BlockColors, BlockFills, ColorOverrides, DensityValues, Fonts, Problem, Provenance,
    ResolvedTheme, TerminalTheme, derive_block_fills, resolve,
};
pub use tokens::{
    COLOR_SPECS, Color, ColorSpec, DENSITY_SPECS, DensityKey, Fallback, FontRole, METRIC_SPECS,
    Metric, NumberScope, NumberSpec, valid_pack_id, valid_token_id,
};

/// `terminal.theme` value that follows the active chrome theme.
pub const AUTO: &str = "auto";
/// Id of the built-in dark pack, the default of `appearance.dark_theme`.
pub const DARK: &str = "hutan-dark";
/// Id of the built-in light pack, the default of `appearance.light_theme`.
pub const LIGHT: &str = "hutan-light";

/// What the OS currently looks like; supplied by the desktop (GPUI knows, this crate does not).
/// Also the polarity of a pack.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SystemAppearance {
    Light,
    Dark,
}
