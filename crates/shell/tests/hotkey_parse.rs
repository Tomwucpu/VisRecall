use visrecall_shell::hotkey::HotkeySpec;

#[test]
fn parses_ctrl_shift_s() {
    let h = HotkeySpec::parse("Ctrl+Shift+S").unwrap();
    assert!(h.ctrl && h.shift && !h.alt && !h.win);
    assert_eq!(h.format(), "Ctrl+Shift+S");
}

#[test]
fn rejects_empty_combo() {
    assert!(HotkeySpec::parse("S").is_err());
}

#[test]
fn rejects_unknown_key() {
    assert!(HotkeySpec::parse("Ctrl+Shift+NoSuchKey").is_err());
}

#[test]
fn accepts_aliases_and_canonicalizes_order() {
    let h = HotkeySpec::parse("s+shift+control+meta").unwrap();
    assert!(h.ctrl && h.shift && h.win && !h.alt);
    assert_eq!(h.format(), "Ctrl+Shift+Win+S");
}

#[test]
fn parses_function_and_named_keys() {
    for (text, formatted) in [
        ("Ctrl+F12", "Ctrl+F12"),
        ("Alt+Space", "Alt+Space"),
        ("Ctrl+Escape", "Ctrl+Esc"),
        ("Ctrl+Left", "Ctrl+Left"),
        ("Ctrl+Up", "Ctrl+Up"),
        ("Ctrl+0", "Ctrl+0"),
    ] {
        let h = HotkeySpec::parse(text).unwrap();
        assert_eq!(h.format(), formatted);
    }
}

#[test]
fn parses_every_single_modifier_and_requires_one() {
    for modifier in ["Ctrl", "Control", "Shift", "Alt", "Win", "Meta"] {
        assert!(HotkeySpec::parse(&format!("{modifier}+A")).is_ok());
        assert!(HotkeySpec::parse(modifier).is_err());
    }
}

#[test]
fn rejects_malformed_combinations() {
    for text in ["", " ", "+", "Ctrl+", "+S", "Ctrl++S", "Ctrl+Shift"] {
        assert!(
            HotkeySpec::parse(text).is_err(),
            "expected {text:?} to be rejected"
        );
    }
}

#[test]
fn register_tuple_has_norepeat_and_expected_codes() {
    let (mods, vk) = HotkeySpec::parse("Ctrl+Shift+S").unwrap().register_tuple();
    // MOD_CONTROL = 0x0002, MOD_SHIFT = 0x0004, MOD_NOREPEAT = 0x4000.
    assert_eq!(mods & 0x0002, 0x0002);
    assert_eq!(mods & 0x0004, 0x0004);
    assert_eq!(mods & 0x4000, 0x4000);
    assert_eq!(vk, 0x53);

    let (mods, vk) = HotkeySpec::parse("Win+F1").unwrap().register_tuple();
    // MOD_WIN = 0x0008, VK_F1 = 0x70.
    assert_eq!(mods, 0x0008 | 0x4000);
    assert_eq!(vk, 0x70);
}
