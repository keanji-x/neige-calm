//! POSIX command argument construction shared by server command builders.

/// POSIX single-quote escaping: `'` → `'\''`.
pub(crate) fn sh_single_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

#[cfg(test)]
mod quote_contract_tests {
    #[test]
    fn posix_quote_preserves_command_bytes() {
        for (input, expected) in [
            ("", "''"),
            ("plain", "'plain'"),
            ("hello", "'hello'"),
            ("she said 'hi'", "'she said '\\''hi'\\'''"),
            (
                "$(rm -rf /) `whoami` ; echo pwned && true | cat",
                "'$(rm -rf /) `whoami` ; echo pwned && true | cat'",
            ),
            ("two words", "'two words'"),
            ("a'b", "'a'\\''b'"),
            ("''", "''\\'''\\'''"),
            ("$`\\\n", "'$`\\\n'"),
        ] {
            assert_eq!(super::sh_single_quote(input), expected, "{input:?}");
        }
    }
}
