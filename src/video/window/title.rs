// SPDX-License-Identifier: GPL-3.0-or-later

//! One source for the normal, captured, and debugger window titles.

use super::*;

fn about_rom_hint(lines: &[String]) -> Option<String> {
    lines
        .iter()
        .find_map(|line| line.strip_prefix("ROM: ").map(str::to_owned))
}

impl App {
    pub(super) fn record_rom_title_hint(&mut self) {
        self.title_rom_hint = about_rom_hint(&self.about_machine_lines)
            .map(|name| (self.emu.machine_descriptor().rom, name));
    }

    fn rom_title(&self) -> String {
        let rom = &self.emu.bus().mem.rom;
        if let Some(id) = crate::romdb::describe(rom) {
            return id.label().to_string();
        }
        let fingerprint = self.emu.machine_descriptor().rom;
        let name = self
            .title_rom_hint
            .as_ref()
            .filter(|(id, _)| *id == fingerprint)
            .map(|(_, name)| name.as_str())
            .unwrap_or("ROM");
        format!("{name} ({:08x})", fingerprint.crc32)
    }

    pub(super) fn base_window_title(&self) -> String {
        if let Some(brand) = crate::video::branding_title() {
            return brand.to_string();
        }
        if self
            .about_machine_lines
            .iter()
            .any(|line| line == crate::config::ABOUT_PLACEHOLDER_LINE)
        {
            return window_title().to_string();
        }
        let descriptor = self.emu.machine_descriptor();
        let machine = descriptor
            .machine
            .map(|model| format!("{model:?}"))
            // An unnamed configuration starts from the A500 Rev 6A profile.
            .unwrap_or_else(|| "A500".to_string());
        let rom = self.rom_title();
        let hash = format!("{:08x}", descriptor.rom.crc32);
        let values = crate::config::window_title::TitleValues {
            app: "Copperline",
            version: env!("COPPERLINE_DISPLAY_VERSION"),
            machine: &machine,
            rom: &rom,
            hash: &hash,
        };
        let template = self
            .machine_config
            .display
            .title
            .as_deref()
            .unwrap_or(crate::config::window_title::DEFAULT_TEMPLATE);
        crate::config::window_title::render(template, &values)
            .unwrap_or_else(|_| window_title().to_string())
    }

    pub(super) fn refresh_window_title(&self) {
        if let Some(r) = &self.render {
            let base = self.base_window_title();
            let title = if self.mouse_captured {
                format!("{base} - Mouse captured ({HOST_SHORTCUT_MODIFIER_LABEL}+G releases)")
            } else if self.debug_layout_active {
                format!("{base} · Debug")
            } else {
                base
            };
            r.window.set_title(&title);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::about_rom_hint;

    #[test]
    fn finds_primary_rom_without_extended_rom() {
        assert_eq!(
            about_rom_hint(&["Extended ROM: other".into(), "ROM: AROS".into()]),
            Some("AROS".into())
        );
    }

    #[test]
    fn title_tracks_live_rom_and_custom_template() {
        let mut app = super::super::tests::test_app();
        let mut descriptor = app.emu.machine_descriptor().clone();
        descriptor.machine = Some(crate::config::MachineModel::A1200);
        app.emu.set_machine_descriptor(descriptor.clone());
        app.about_machine_lines = vec!["ROM: AROS 1.0".into()];
        app.record_rom_title_hint();
        let hash = format!("{:08x}", app.emu.machine_descriptor().rom.crc32);
        let title = app.base_window_title();
        assert!(title.contains("A1200"));
        assert!(title.contains(&format!("AROS 1.0 ({hash})")));

        app.machine_config.display.title = Some("Test {m}: {r} [{hash}]".into());
        assert_eq!(
            app.base_window_title(),
            format!("Test A1200: AROS 1.0 ({hash}) [{hash}]")
        );

        // A state load may replace the ROM without a host path to identify it.
        app.emu.bus_mut().mem.rom[32] ^= 1;
        app.emu.set_machine_descriptor(descriptor);
        assert!(app.base_window_title().contains("ROM ("));
        assert!(!app.base_window_title().contains("AROS 1.0"));
    }
}
