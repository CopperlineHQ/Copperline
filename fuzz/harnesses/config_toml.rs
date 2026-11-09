//! Parse configuration without resolving paths, opening media or starting
//! devices named by the input. Shared by libFuzzer and cargo test replay.

pub fn exercise(data: &[u8]) {
    if let Ok(text) = std::str::from_utf8(data) {
        if let Ok(raw) = copperline::config::RawConfig::parse(text) {
            let _ = raw.paths();
            let _ = raw.menu_scale();
            let _ = raw.track_display();
            let _ = raw.audio_output_enabled();
        }
    }
}
