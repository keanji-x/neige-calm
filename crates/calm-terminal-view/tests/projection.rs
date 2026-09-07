use calm_terminal_view::{Rasterizer, TerminalView, click_bytes, key_bytes};

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
    assert!(click_bytes(1, 1, &frame).is_err());
    view.feed(b"\x1b[?1000h\x1b[?1006h");
    assert_eq!(
        click_bytes(1, 1, &view.frame(0).unwrap()).unwrap(),
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
