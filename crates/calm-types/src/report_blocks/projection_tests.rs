use super::*;

#[test]
fn independent_blocks_have_line_boundaries_without_rewriting_content() {
    for (parts, expected) in [
        (vec!["# A\ntext", "# B\nmore"], "# A\ntext\n# B\nmore"),
        (vec!["a\n", "b"], "a\nb"),
        (vec!["a\r\n", "b"], "a\r\nb"),
        (vec!["a\n\n", "b"], "a\n\nb"),
        (vec!["", "a", "", "b", ""], "a\nb"),
        (vec!["last block"], "last block"),
    ] {
        let mut body = String::new();
        for text in parts {
            append_block_text(&mut body, text);
        }
        assert_eq!(body, expected);
    }
}

#[test]
fn projections_of_imported_slices_remain_byte_exact() {
    for source in [
        "",
        "# A\ntext\n# B\nlast",
        "# A\r\ntext\r\n## B\r\n",
        "a\n\n# B\n",
        "text\n```neige-block app\n{\"src\":\"/x\"}\n```\n# Tail",
    ] {
        let mut body = String::new();
        for slice in split_body(source) {
            append_block_text(&mut body, &slice.raw);
        }
        assert_eq!(body, source);
    }
}
