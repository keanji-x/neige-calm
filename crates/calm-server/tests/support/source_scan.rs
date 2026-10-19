/// Only a file-level unconditional `cfg(test)` proves that no caller in the file can
/// enter a production build. File names, comments and attributes on individual items do not.
pub fn is_test_only_source(source: &str) -> bool {
    syn::parse_file(source)
        .expect("parse source for file-level test configuration")
        .attrs
        .iter()
        .any(|attr| {
            attr.path().is_ident("cfg")
                && attr
                    .parse_args::<syn::Path>()
                    .is_ok_and(|path| path.is_ident("test"))
        })
}
