use hutan_syntax::{Language, Snapshot};

#[test]
fn every_compiled_language_loads() {
    for language in Language::compiled() {
        let result = Snapshot::parse(language, String::new(), 0);
        assert!(result.is_ok(), "{language}: {:?}", result.err());
    }
}
