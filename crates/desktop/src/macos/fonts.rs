//! The font families installed on this Mac, for the pickers of the Settings sheet
//! (`terminal.font_family`): their names, which of them are monospaced, the font file *Install
//! font…* adds to the user's account, and the system's word that the installed set changed.
//!
//! The workbench reads the list off the main thread (`families` blocks while CoreText answers),
//! keeps it and reads it again when [`FontSource::observe`] says fonts were added or removed.
//! Tests use [`FakeFonts`]: they never ask CoreText and never register a font into the account.

use std::any::Any;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use tokio::sync::mpsc;

/// One installed font family.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FontFamily {
    pub name: String,
    /// The family's regular face is fixed-pitch (`kCTFontMonoSpaceTrait`).
    pub monospaced: bool,
}

impl FontFamily {
    pub fn new(name: impl Into<String>, monospaced: bool) -> Self {
        Self {
            name: name.into(),
            monospaced,
        }
    }
}

/// Font file extensions *Install font…* accepts (CoreText reads these; a collection adds all
/// its fonts).
pub const FONT_EXTENSIONS: [&str; 4] = ["ttf", "otf", "ttc", "otc"];

/// Whether `path` names a font file by its extension.
pub fn is_font_file(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| FONT_EXTENSIONS.iter().any(|x| e.eq_ignore_ascii_case(x)))
}

/// Names for display: no hidden system families (`.SF NS`), no duplicates, sorted without
/// regard to case.
pub fn tidy(mut families: Vec<FontFamily>) -> Vec<FontFamily> {
    families.retain(|f| !f.name.is_empty() && !f.name.starts_with('.'));
    families.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then_with(|| a.name.cmp(&b.name))
    });
    families.dedup_by(|a, b| a.name == b.name);
    families
}

/// The system's fonts.
pub trait FontSource: Send + Sync {
    /// Every installed family. Blocks: call it off the main thread.
    fn families(&self) -> Vec<FontFamily>;
    /// Adds the font file at `path` to the user's account (it stays installed, like a font
    /// added in Font Book). `Err` is a sentence for the user.
    fn install(&self, path: &Path) -> Result<(), String>;
    /// Sends `()` on `changed` whenever fonts are added or removed (the receiver reads
    /// [`families`](Self::families) again). The send never blocks: a full queue already holds
    /// a pending re-read. The observer lives until the returned value is dropped; `None` when
    /// the system cannot tell.
    fn observe(&self, changed: mpsc::Sender<()>) -> Option<Box<dyn Any>>;
}

/// No fonts, and no way to add one: what a workbench has unless it is given the system's.
#[derive(Debug, Default, Clone, Copy)]
pub struct Nothing;

impl FontSource for Nothing {
    fn families(&self) -> Vec<FontFamily> {
        Vec::new()
    }

    fn install(&self, _path: &Path) -> Result<(), String> {
        Err("Installing fonts is not available here".to_owned())
    }

    fn observe(&self, _changed: mpsc::Sender<()>) -> Option<Box<dyn Any>> {
        None
    }
}

#[derive(Debug, Default)]
struct FakeState {
    families: Vec<FontFamily>,
    installed: Vec<PathBuf>,
    refuse: Option<String>,
    observers: Vec<mpsc::Sender<()>>,
    reads: usize,
}

/// A list of families the test controls. Cloned handles share one state; every clone the
/// workbench holds sees what the test sets.
#[derive(Debug, Clone, Default)]
pub struct FakeFonts {
    state: Arc<Mutex<FakeState>>,
}

impl FakeFonts {
    pub fn new(families: Vec<FontFamily>) -> Self {
        let fake = Self::default();
        fake.lock().families = families;
        fake
    }

    fn lock(&self) -> MutexGuard<'_, FakeState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The system's installed set changed: the families are now `families`, and observers are
    /// told.
    pub fn set_families(&self, families: Vec<FontFamily>) {
        let observers = {
            let mut state = self.lock();
            state.families = families;
            state.observers.clone()
        };
        for observer in observers {
            let _ = observer.try_send(());
        }
    }

    /// The font files `install` was asked to add, oldest first.
    pub fn installed(&self) -> Vec<PathBuf> {
        self.lock().installed.clone()
    }

    /// Makes `install` fail with `message` (`None`: succeed again).
    pub fn refuse_installs(&self, message: Option<&str>) {
        self.lock().refuse = message.map(str::to_owned);
    }

    /// How many times the list was read.
    pub fn reads(&self) -> usize {
        self.lock().reads
    }
}

impl FontSource for FakeFonts {
    fn families(&self) -> Vec<FontFamily> {
        let mut state = self.lock();
        state.reads += 1;
        state.families.clone()
    }

    /// Records the file and adds a proportional family named after it.
    fn install(&self, path: &Path) -> Result<(), String> {
        {
            let mut state = self.lock();
            if let Some(message) = &state.refuse {
                return Err(message.clone());
            }
            state.installed.push(path.to_path_buf());
        }
        let name = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let mut families = self.lock().families.clone();
        families.push(FontFamily::new(name, false));
        self.set_families(families);
        Ok(())
    }

    fn observe(&self, changed: mpsc::Sender<()>) -> Option<Box<dyn Any>> {
        self.lock().observers.push(changed);
        None
    }
}

/// CoreText: `CTFontManager`'s families, the monospace trait of each one's regular face,
/// registration of a font file, and its change notification.
#[cfg(target_os = "macos")]
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemFonts;

#[cfg(target_os = "macos")]
mod system {
    use std::any::Any;
    use std::path::Path;
    use std::ptr::NonNull;

    use objc2::rc::Retained;
    use objc2::runtime::ProtocolObject;
    use objc2_core_foundation::{CFArray, CFError, CFRetained, CFString, CFURL};
    use objc2_core_text::{
        CTFont, CTFontManagerCopyAvailableFontFamilyNames, CTFontManagerRegisterFontsForURL,
        CTFontManagerScope, CTFontSymbolicTraits, kCTFontManagerRegisteredFontsChangedNotification,
    };
    use objc2_foundation::{
        NSDistributedNotificationCenter, NSNotification, NSNotificationCenter, NSObjectProtocol,
        NSString,
    };
    use tokio::sync::mpsc;

    use super::{FontFamily, FontSource, SystemFonts, is_font_file, tidy};

    /// Sizes the probe font is made at; the trait does not depend on it.
    const PROBE_SIZE: f64 = 12.0;

    impl FontSource for SystemFonts {
        fn families(&self) -> Vec<FontFamily> {
            // SAFETY: a plain CoreText call with no arguments; it may be made from any thread.
            #[allow(unsafe_code)]
            let names = unsafe { CTFontManagerCopyAvailableFontFamilyNames() };
            // SAFETY: the array holds CFStrings (the function's contract).
            #[allow(unsafe_code)]
            let names: &CFArray<CFString> = unsafe { names.cast_unchecked() };
            let families = names
                .iter()
                .map(|name| {
                    let name_text = name.to_string();
                    // SAFETY: `name` is a live CFString and a null matrix means identity.
                    #[allow(unsafe_code)]
                    let font = unsafe { CTFont::with_name(&name, PROBE_SIZE, std::ptr::null()) };
                    // SAFETY: `font` is a live CTFont.
                    #[allow(unsafe_code)]
                    let traits = unsafe { font.symbolic_traits() };
                    FontFamily::new(
                        name_text,
                        traits.contains(CTFontSymbolicTraits::MonoSpaceTrait),
                    )
                })
                .collect();
            tidy(families)
        }

        fn install(&self, path: &Path) -> Result<(), String> {
            if !is_font_file(path) {
                return Err("Choose a .ttf, .otf, .ttc or .otc font file".to_owned());
            }
            let url = CFURL::from_file_path(path)
                .ok_or_else(|| format!("{} is not a usable path", path.display()))?;
            let mut error: *mut CFError = std::ptr::null_mut();
            // SAFETY: `url` is a live file URL; `error` is a valid out pointer.
            #[allow(unsafe_code)]
            let registered = unsafe {
                CTFontManagerRegisterFontsForURL(&url, CTFontManagerScope::User, &raw mut error)
            };
            if registered {
                return Ok(());
            }
            // SAFETY: on failure CoreText hands over one retained CFError (or null).
            #[allow(unsafe_code)]
            let reason = NonNull::new(error)
                .map(|error| unsafe { CFRetained::from_raw(error) })
                .and_then(|error| error.description())
                .map(|text| text.to_string());
            Err(match reason {
                Some(reason) => format!("Could not install {}: {reason}", path.display()),
                None => format!("Could not install {}", path.display()),
            })
        }

        fn observe(&self, changed: mpsc::Sender<()>) -> Option<Box<dyn Any>> {
            // Fonts of the user's account change the distributed center; those of this process,
            // the local one.
            // SAFETY: a constant CFString that lives as long as CoreText.
            #[allow(unsafe_code)]
            let name = NSString::from_str(
                &unsafe { kCTFontManagerRegisteredFontsChangedNotification }.to_string(),
            );
            let local = NSNotificationCenter::defaultCenter();
            let distributed: Retained<NSNotificationCenter> =
                Retained::into_super(NSDistributedNotificationCenter::defaultCenter());
            let watch = |center: Retained<NSNotificationCenter>| {
                let changed = changed.clone();
                let block = block2::RcBlock::new(move |_: NonNull<NSNotification>| {
                    // Runs on the posting thread.
                    let _ = changed.try_send(());
                });
                // SAFETY: the block only calls `try_send` on a `Sender`, which is `Send` +
                // `Sync`; the name is a valid string; no queue means the block runs where it is
                // posted.
                #[allow(unsafe_code)]
                let token = unsafe {
                    center.addObserverForName_object_queue_usingBlock(
                        Some(&name),
                        None,
                        None,
                        &block,
                    )
                };
                (center, token)
            };
            let observers = [watch(local), watch(distributed)];
            Some(Box::new(FontObserver { observers }))
        }
    }

    /// Removes the observers when dropped.
    struct FontObserver {
        observers: [(
            Retained<NSNotificationCenter>,
            Retained<ProtocolObject<dyn NSObjectProtocol>>,
        ); 2],
    }

    impl Drop for FontObserver {
        fn drop(&mut self) {
            for (center, token) in &self.observers {
                // SAFETY: `token` is what `addObserverForName…` returned for this center.
                #[allow(unsafe_code)]
                unsafe {
                    center.removeObserver((**token).as_ref());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn family(name: &str, mono: bool) -> FontFamily {
        FontFamily::new(name, mono)
    }

    #[test]
    fn the_list_is_sorted_without_case_deduplicated_and_without_hidden_families() {
        let tidy = tidy(vec![
            family("menlo", true),
            family(".SF NS", false),
            family("Zapfino", false),
            family("Avenir", false),
            family("Avenir", false),
            family("", false),
            family("Menlo", true),
        ]);
        let names: Vec<&str> = tidy.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["Avenir", "Menlo", "menlo", "Zapfino"]);
    }

    #[test]
    fn font_files_are_told_by_their_extension() {
        for yes in ["a.ttf", "A.OTF", "/x/y/z.ttc", "f.otc"] {
            assert!(is_font_file(Path::new(yes)), "{yes}");
        }
        for no in ["a.woff2", "font", "a.ttf.zip", "a."] {
            assert!(!is_font_file(Path::new(no)), "{no}");
        }
    }

    #[test]
    fn the_fake_installs_and_tells_its_observers() {
        let fake = FakeFonts::new(vec![family("Menlo", true)]);
        let (sender, mut receiver) = mpsc::channel(1);
        assert!(fake.observe(sender).is_none());
        fake.install(Path::new("/tmp/Scratch Mono.ttf")).unwrap();
        assert_eq!(receiver.try_recv(), Ok(()));
        assert_eq!(fake.installed(), [PathBuf::from("/tmp/Scratch Mono.ttf")]);
        let names: Vec<String> = fake.families().into_iter().map(|f| f.name).collect();
        assert_eq!(names, ["Menlo", "Scratch Mono"]);
        assert_eq!(fake.reads(), 1);

        fake.refuse_installs(Some("no"));
        assert_eq!(fake.install(Path::new("/tmp/x.ttf")), Err("no".to_owned()));
        assert_eq!(fake.installed().len(), 1);
    }

    /// CoreText on the machine that runs the tests: Menlo and Helvetica ship with macOS.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_system_lists_monospaced_and_proportional_families() {
        let families = SystemFonts.families();
        let find = |name: &str| families.iter().find(|f| f.name == name);
        assert_eq!(find("Menlo").map(|f| f.monospaced), Some(true));
        assert_eq!(find("Helvetica").map(|f| f.monospaced), Some(false));
        assert!(families.iter().all(|f| !f.name.starts_with('.')));
        assert!(
            families
                .windows(2)
                .all(|w| w[0].name.to_lowercase() <= w[1].name.to_lowercase())
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn the_system_refuses_what_is_not_a_font_file_and_a_missing_one() {
        assert!(SystemFonts.install(Path::new("/tmp/notes.txt")).is_err());
        let missing = SystemFonts.install(Path::new("/nonexistent/hutan-no-such-font.ttf"));
        assert!(missing.is_err(), "{missing:?}");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn the_system_observer_wakes_on_the_fonts_notification_until_it_is_dropped() {
        use objc2_foundation::{NSNotificationCenter, NSString};
        let (sender, mut receiver) = mpsc::channel(4);
        let observer = SystemFonts.observe(sender).expect("macOS can tell");
        let center = NSNotificationCenter::defaultCenter();
        // SAFETY: a constant CFString that lives as long as CoreText.
        #[allow(unsafe_code)]
        let name = NSString::from_str(
            &unsafe { objc2_core_text::kCTFontManagerRegisteredFontsChangedNotification }
                .to_string(),
        );
        // SAFETY: a name, no object, no user info.
        #[allow(unsafe_code)]
        let post = || unsafe { center.postNotificationName_object(&name, None) };
        post();
        assert_eq!(receiver.try_recv(), Ok(()));
        drop(observer);
        post();
        // The observer's blocks went with it, and the channel with them: no more wake-ups.
        assert_eq!(
            receiver.try_recv(),
            Err(mpsc::error::TryRecvError::Disconnected),
            "a dropped observer stays silent"
        );
    }
}
