use calm_terminal_view::{Occurrence, Rasterizer, TerminalView, click_bytes, key_bytes};

#[test]
fn cjk_highlight_and_key_modes_share_one_projection() {
    let mut view = TerminalView::new(80, 24, [220, 220, 220], [15, 20, 24]).unwrap();
    view.feed("\x1b[7m/status 中文\x1b[0m\r\n> /s\x1b[?1h".as_bytes());
    let frame = view.frame(0).unwrap();
    assert_eq!(frame.text[0], "/status 中文");
    assert_eq!(frame.cursor.column, 4);
    assert_eq!(frame.cursor.row, 1);
    assert_eq!(frame.cells[8].width, 2);
    assert_eq!(frame.cells[9].width, 0);
    assert_ne!(frame.cells[0].attributes & 16, 0);
    assert_eq!(key_bytes("Up", frame.modes).unwrap(), b"\x1bOA");
    assert!(click_bytes(1, 1, &frame.input_surface()).is_err());
    view.feed(b"\x1b[?1000h\x1b[?1006h");
    assert_eq!(
        click_bytes(1, 1, &view.frame(0).unwrap().input_surface()).unwrap(),
        b"\x1b[<0;2;2M\x1b[<0;2;2m"
    );
}
#[test]
fn history_scroll_is_read_only_and_alternate_screen_restores() {
    let mut view = TerminalView::new(20, 3, [220, 220, 220], [15, 20, 24]).unwrap();
    for i in 0..20 {
        view.feed(format!("line{i}\r\n").as_bytes());
    }
    let live = view.frame(0).unwrap();
    let previous = view.frame(5).unwrap();
    assert_ne!(live.text, previous.text);
    assert!(!previous.cursor.visible);
    assert_eq!(view.frame(0).unwrap().text, live.text);
    view.feed(b"\x1b[?1049h\x1b[2J\x1b[Hmenu");
    assert!(view.frame(5).unwrap().alternate);
    assert_eq!(view.frame(5).unwrap().scroll_offset, 0);
    view.feed(b"\x1b[?1049l");
    assert_eq!(view.frame(0).unwrap().text, live.text);
}
/// rmux stores an attributed line to its written extent and pads only plain lines, so a history view must pad the missing cells itself.
#[test]
fn history_view_pads_attributed_lines_stored_to_their_written_extent() {
    let mut view = TerminalView::new(20, 3, [220, 220, 220], [15, 20, 24]).unwrap();
    for i in 0..6 {
        view.feed(format!("\x1b[31mred{i}\x1b[0m\r\n").as_bytes());
    }
    let live = view.frame(0).unwrap();
    assert_eq!(live.history_rows, 4);
    assert_eq!(live.text, ["red4", "red5", ""]);
    // The live viewport is always full width; its blank cells are the reference for a padded history cell.
    let blank = &live.cells[live.cells.len() - 1];
    assert_eq!(blank.text, " ");
    for offset in 1..=live.history_rows {
        let frame = view
            .frame(offset)
            .unwrap_or_else(|error| panic!("offset {offset}: {error}"));
        assert_eq!(frame.scroll_offset, offset);
        assert_eq!(frame.cells.len(), 20 * 3);
        let expected: Vec<String> = (0..3)
            .map(|row| format!("red{}", 4 - offset + row))
            .collect();
        assert_eq!(frame.text, expected, "offset {offset}");
        let padded = &frame.cells[4];
        assert_eq!(padded.text, blank.text, "offset {offset}");
        assert_eq!(padded.width, blank.width, "offset {offset}");
        assert_eq!(padded.attributes, blank.attributes, "offset {offset}");
        assert_eq!(padded.foreground, blank.foreground, "offset {offset}");
        assert_eq!(padded.background, blank.background, "offset {offset}");
        assert_ne!(frame.cells[0].foreground, blank.foreground);
    }
}
#[test]
fn find_text_locates_history_and_live_rows_by_occurrence() {
    let mut view = TerminalView::new(20, 3, [220, 220, 220], [15, 20, 24]).unwrap();
    for i in 0..40 {
        let marker = if i % 10 == 0 { " MARK" } else { "" };
        view.feed(format!("line{i}{marker}\r\n").as_bytes());
    }
    view.feed(b"prompt> ");
    let live = view.frame(0).unwrap();
    let history = live.history_rows;
    assert!(history >= 30, "{history}");
    // Nothing scrolled out of the 2000-row scrollback, so line k sits at absolute row k.
    assert_eq!(view.find_text("MARK", Occurrence::Latest), Some(30));
    assert_eq!(view.find_text("MARK", Occurrence::Earliest), Some(0));
    assert_eq!(view.find_text("line7", Occurrence::Latest), Some(7));
    assert_eq!(view.find_text("absent", Occurrence::Latest), None);
    assert_eq!(view.find_text("absent", Occurrence::Earliest), None);
    let scrolled = view.frame(history - 30).unwrap();
    assert_eq!(scrolled.text[0], "line30 MARK");
    let prompt = view.find_text("prompt>", Occurrence::Latest).unwrap();
    assert!(prompt >= history, "{prompt} vs {history}");
    assert_eq!(live.text[prompt - history], "prompt>");
    assert_eq!(view.find_text("mark", Occurrence::Latest), None);
    // The plain text is per row, so an SGR boundary inside the pattern still matches.
    view.feed(b"\r\n\x1b[31msplit\x1b[0m-\x1b[1mword\x1b[0m\r\nA\r\nB\r\nC\r\n");
    let split = view.find_text("split-word", Occurrence::Latest).unwrap();
    let history = view.history_rows();
    assert!(split < history, "{split} vs {history}");
    assert_eq!(view.frame(history - split).unwrap().text[0], "split-word");
    view.feed(b"\x1b[?1049h\x1b[2J\x1b[Hmenu MARK");
    let history = view.history_rows();
    assert_eq!(view.find_text("line30", Occurrence::Latest), None);
    assert_eq!(view.find_text("MARK", Occurrence::Earliest), Some(history));
    assert_eq!(view.find_text("MARK", Occurrence::Latest), Some(history));
    view.feed(b"\x1b[?1049l");
    assert_eq!(view.find_text("MARK", Occurrence::Latest), Some(30));
}
#[test]
fn raster_escapes_terminal_markup_and_emits_bounded_png() {
    let mut view = TerminalView::new(80, 24, [220, 220, 220], [15, 20, 24]).unwrap();
    view.feed("\x1b[7m/status 中文\x1b[0m\r\n<script>&'\"".as_bytes());
    let raster = Rasterizer::system().unwrap();
    let bytes = raster.png(&view.frame(0).unwrap()).unwrap();
    assert!(bytes.starts_with(b"\x89PNG\r\n\x1a\n"));
    assert!(bytes.len() > 1000 && bytes.len() < 2 * 1024 * 1024);
    assert_eq!(u32::from_be_bytes(bytes[16..20].try_into().unwrap()), 800);
    assert_eq!(u32::from_be_bytes(bytes[20..24].try_into().unwrap()), 480);
}
