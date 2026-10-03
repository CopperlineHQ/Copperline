// SPDX-License-Identifier: GPL-3.0-or-later

//! Monitor enumeration and placement. Resolve handles afresh when applying a
//! preference: a saved choice must not keep a disconnected display alive.

use super::*;
use crate::config::HostMonitor;
use anyhow::bail;
use winit::{
    dpi::{PhysicalPosition, Position},
    monitor::MonitorHandle,
};

fn selected_index(
    selection: &HostMonitor,
    names: &[Option<String>],
    primary: Option<usize>,
) -> Result<Option<usize>> {
    match selection {
        HostMonitor::Auto => Ok(None),
        HostMonitor::Primary => primary
            .map(Some)
            .ok_or_else(|| anyhow!("the host did not report a primary monitor")),
        HostMonitor::Index(number) => number
            .checked_sub(1)
            .filter(|&index| index < names.len())
            .map(Some)
            .ok_or_else(|| anyhow!("monitor {number} is unavailable")),
        HostMonitor::Name(name) => {
            let mut matches = names
                .iter()
                .enumerate()
                .filter(|(_, candidate)| candidate.as_deref() == Some(name.as_str()));
            let index = matches
                .next()
                .map(|(index, _)| index)
                .ok_or_else(|| anyhow!("monitor {name:?} is unavailable"))?;
            if matches.next().is_some() {
                bail!("monitor name {name:?} is ambiguous; select a number from --list-monitors");
            }
            Ok(Some(index))
        }
    }
}

pub(super) fn resolve(
    selection: &HostMonitor,
    monitors: &[MonitorHandle],
    primary: Option<MonitorHandle>,
) -> Option<MonitorHandle> {
    let names: Vec<_> = monitors.iter().map(MonitorHandle::name).collect();
    let primary = primary.and_then(|p| monitors.iter().position(|m| *m == p));
    match selected_index(selection, &names, primary) {
        Ok(index) => index.map(|index| monitors[index].clone()),
        Err(error) => {
            warn!("host monitor {selection}: {error}; using automatic placement");
            None
        }
    }
}

fn choice(index: usize, names: &[Option<String>]) -> HostMonitor {
    match &names[index] {
        Some(name)
            if !name.is_empty()
                && names
                    .iter()
                    .filter(|n| n.as_deref() == Some(name.as_str()))
                    .count()
                    == 1 =>
        {
            HostMonitor::Name(name.clone())
        }
        _ => HostMonitor::Index(index + 1),
    }
}

pub(super) fn choices(monitors: &[MonitorHandle]) -> Vec<(HostMonitor, String)> {
    let names: Vec<_> = monitors.iter().map(MonitorHandle::name).collect();
    monitors
        .iter()
        .enumerate()
        .map(|(index, monitor)| {
            let size = monitor.size();
            let name = names[index].as_deref().unwrap_or("Unnamed display");
            (
                choice(index, &names),
                format!("{}: {name} ({}x{})", index + 1, size.width, size.height),
            )
        })
        .collect()
}

#[cfg(any(not(target_os = "macos"), test))]
fn centered_position(
    origin: PhysicalPosition<i32>,
    monitor: PhysicalSize<u32>,
    window: PhysicalSize<u32>,
) -> PhysicalPosition<i32> {
    let axis = |origin: i32, extent: u32, window: u32| {
        (i64::from(origin) + i64::from(extent.saturating_sub(window)) / 2)
            .clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
    };
    PhysicalPosition::new(
        axis(origin.x, monitor.width, window.width),
        axis(origin.y, monitor.height, window.height),
    )
}

pub(super) fn initial_position(monitor: &MonitorHandle, size: LogicalSize<f64>) -> Position {
    #[cfg(target_os = "macos")]
    {
        macos_position(
            monitor.position(),
            monitor.size(),
            monitor.scale_factor(),
            size,
        )
        .into()
    }
    #[cfg(not(target_os = "macos"))]
    {
        centered_position(
            monitor.position(),
            monitor.size(),
            size.to_physical(monitor.scale_factor()),
        )
        .into()
    }
}

// winit's macOS physical positions are scaled by each screen/window's own
// factor. Cross-screen placement must use the common logical desktop space:
// a target's physical origin interpreted with the old scale can hit another screen.
#[cfg(any(target_os = "macos", test))]
fn macos_position(
    origin: PhysicalPosition<i32>,
    monitor: PhysicalSize<u32>,
    monitor_scale: f64,
    window: LogicalSize<f64>,
) -> winit::dpi::LogicalPosition<f64> {
    let origin = origin.to_logical::<f64>(monitor_scale);
    let monitor = monitor.to_logical::<f64>(monitor_scale);
    winit::dpi::LogicalPosition::new(
        origin.x + (monitor.width - window.width).max(0.0) / 2.0,
        origin.y + (monitor.height - window.height).max(0.0) / 2.0,
    )
}

pub(super) fn windowed_placement_supported(window: &Window) -> bool {
    let _ = window;
    #[cfg(target_os = "linux")]
    {
        use pixels::raw_window_handle::{HasWindowHandle, RawWindowHandle};
        if matches!(
            window.window_handle().map(|handle| handle.as_raw()),
            Ok(RawWindowHandle::Wayland(_))
        ) {
            warn!("Wayland controls windowed monitor placement; the selected monitor applies to fullscreen");
            return false;
        }
    }
    true
}

pub(super) fn place_window(window: &Window, monitor: &MonitorHandle) {
    if !windowed_placement_supported(window) {
        return;
    }
    if window.is_maximized() {
        window.set_maximized(false);
    }
    #[cfg(target_os = "macos")]
    window.set_outer_position(macos_position(
        monitor.position(),
        monitor.size(),
        monitor.scale_factor(),
        window.outer_size().to_logical(window.scale_factor()),
    ));
    #[cfg(not(target_os = "macos"))]
    window.set_outer_position(centered_position(
        monitor.position(),
        monitor.size(),
        window.outer_size(),
    ));
}

/// List host displays without loading a ROM, constructing a machine, or
/// creating a GPU surface. Enumeration runs on winit's UI thread.
pub fn print_monitors() -> Result<()> {
    struct ListMonitors;
    impl ApplicationHandler for ListMonitors {
        fn resumed(&mut self, event_loop: &ActiveEventLoop) {
            let monitors: Vec<_> = event_loop.available_monitors().collect();
            let primary = event_loop.primary_monitor();
            println!("Host monitors (for --monitor / [display] monitor):");
            if monitors.is_empty() {
                println!("  (none found)");
            }
            for (monitor, (_, label)) in monitors.iter().zip(choices(&monitors)) {
                let marker = if primary.as_ref() == Some(monitor) {
                    " [primary]"
                } else {
                    ""
                };
                println!("  {label}, scale {}{marker}", monitor.scale_factor());
            }
            event_loop.exit();
        }

        fn window_event(&mut self, _: &ActiveEventLoop, _: WindowId, _: WindowEvent) {}
    }
    EventLoop::new()?.run_app(&mut ListMonitors)?;
    Ok(())
}

impl App {
    pub(super) fn selected_host_monitor(&self) -> Option<MonitorHandle> {
        if self.host_monitor == HostMonitor::Auto {
            return None;
        }
        let window = &self.render.as_ref()?.window;
        resolve(
            &self.host_monitor,
            &window.available_monitors().collect::<Vec<_>>(),
            window.primary_monitor(),
        )
    }

    pub(super) fn refresh_launcher_monitors(&mut self) {
        let Some(render) = &self.render else { return };
        let choices = choices(&render.window.available_monitors().collect::<Vec<_>>());
        if let Some(state) = self.launcher_state_mut() {
            state.setup.set_host_monitors(choices);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selection_handles_missing_primary_disconnected_and_duplicate_names() {
        let names = vec![Some("Internal".into()), Some("External".into())];
        assert_eq!(
            selected_index(&HostMonitor::Auto, &names, None).unwrap(),
            None
        );
        assert_eq!(
            selected_index(&HostMonitor::Index(2), &names, None).unwrap(),
            Some(1)
        );
        assert_eq!(
            selected_index(&HostMonitor::Primary, &names, Some(1)).unwrap(),
            Some(1)
        );
        assert_eq!(
            selected_index(&HostMonitor::Name("External".into()), &names, None).unwrap(),
            Some(1)
        );
        for selection in [
            HostMonitor::Primary,
            HostMonitor::Index(0),
            HostMonitor::Index(3),
            HostMonitor::Name("Disconnected".into()),
        ] {
            assert!(selected_index(&selection, &names, None).is_err());
        }
        let duplicates = vec![Some("Same".into()), Some("Same".into()), None];
        assert!(selected_index(&HostMonitor::Name("Same".into()), &duplicates, None).is_err());
        assert_eq!(choice(0, &duplicates), HostMonitor::Index(1));
        assert_eq!(choice(2, &duplicates), HostMonitor::Index(3));
        assert_eq!(choice(1, &names), HostMonitor::Name("External".into()));
        assert!(selected_index(&HostMonitor::Index(1), &[], None).is_err());
    }

    #[test]
    fn centering_uses_physical_coordinates_and_keeps_oversized_windows_reachable() {
        assert_eq!(
            centered_position(
                PhysicalPosition::new(-1920, -200),
                PhysicalSize::new(1920, 1080),
                PhysicalSize::new(800, 600)
            ),
            PhysicalPosition::new(-1360, 40)
        );
        assert_eq!(
            centered_position(
                PhysicalPosition::new(1920, 0),
                PhysicalSize::new(1280, 720),
                PhysicalSize::new(3000, 2000)
            ),
            PhysicalPosition::new(1920, 0)
        );
    }

    #[test]
    fn macos_host_monitor_placement_uses_target_and_window_dpi_independently() {
        let window = PhysicalSize::new(1600, 1200).to_logical(2.0);
        let position = macos_position(
            PhysicalPosition::new(-2560, 0),
            PhysicalSize::new(2560, 1440),
            1.0,
            window,
        );
        assert_eq!(position, winit::dpi::LogicalPosition::new(-1680.0, 420.0));
        let position = macos_position(
            PhysicalPosition::new(5120, -400),
            PhysicalSize::new(2880, 1800),
            2.0,
            LogicalSize::new(800.0, 600.0),
        );
        assert_eq!(position, winit::dpi::LogicalPosition::new(2880.0, -50.0));
    }
}
