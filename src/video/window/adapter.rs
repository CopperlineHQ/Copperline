// SPDX-License-Identifier: GPL-3.0-or-later

//! Preserve a working renderer while trying to replace a software adapter.

use pixels::wgpu::{AdapterInfo, Backend, DeviceType};

/// wgpu can prefer a primary backend's CPU adapter over an accelerated GL
/// adapter. Try GL before building any renderer-dependent UI resources, and
/// keep the original renderer alive until its replacement has been built.
/// Explicit backend/adapter selections and non-Windows hosts opt out.
pub(super) fn prefer_hardware_renderer<T, E: std::fmt::Display>(
    renderer: T,
    automatic_fallback: bool,
    adapter_info: impl Fn(&T) -> AdapterInfo,
    build_opengl: impl FnOnce() -> Result<T, E>,
) -> T {
    let initial = adapter_info(&renderer);
    if !automatic_fallback
        || initial.device_type != DeviceType::Cpu
        || initial.backend == Backend::Gl
    {
        return renderer;
    }

    log::info!(
        "window adapter {:?} ({:?}) uses software rendering; trying OpenGL",
        initial.name,
        initial.backend,
    );
    match build_opengl() {
        Ok(candidate) => {
            let alternative = adapter_info(&candidate);
            if alternative.device_type != DeviceType::Cpu {
                log::info!(
                    "window adapter: using OpenGL adapter {:?} ({:?}) instead of software adapter {:?}",
                    alternative.name,
                    alternative.device_type,
                    initial.name,
                );
                return candidate;
            }
            log::info!(
                "OpenGL adapter {:?} also uses software rendering; keeping {:?}",
                alternative.name,
                initial.name,
            );
        }
        Err(error) => log::warn!(
            "OpenGL renderer unavailable: {error}; keeping software adapter {:?}",
            initial.name,
        ),
    }
    renderer
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    fn adapter(backend: Backend, device_type: DeviceType) -> AdapterInfo {
        AdapterInfo {
            name: String::new(),
            vendor: 0,
            device: 0,
            backend,
            device_type,
            device_pci_bus_id: String::new(),
            driver: String::new(),
            driver_info: String::new(),
            subgroup_min_size: 0,
            subgroup_max_size: 0,
            transient_saves_memory: false,
        }
    }

    #[test]
    fn software_adapter_can_be_replaced_by_an_unclassified_gl_driver() {
        // GL classifies unfamiliar hardware (including virtual drivers) as
        // Other. Requiring DiscreteGpu/IntegratedGpu would miss those drivers.
        let original = adapter(Backend::Dx12, DeviceType::Cpu);
        let replacement = adapter(Backend::Gl, DeviceType::Other);
        let selected =
            prefer_hardware_renderer(original, true, Clone::clone, || Ok::<_, &str>(replacement));
        assert_eq!(selected.backend, Backend::Gl);
        assert_eq!(selected.device_type, DeviceType::Other);
    }

    #[test]
    fn failed_or_software_gl_candidate_keeps_the_working_renderer() {
        for candidate in [
            Err("no compatible GL surface"),
            Err("GL device creation failed"),
            Ok(adapter(Backend::Gl, DeviceType::Cpu)),
        ] {
            let original = adapter(Backend::Dx12, DeviceType::Cpu);
            let selected =
                prefer_hardware_renderer(original.clone(), true, Clone::clone, || candidate);
            assert_eq!(selected, original);
        }
    }

    #[test]
    fn explicit_selection_and_existing_gpu_do_not_initialize_another_backend() {
        let attempts = Cell::new(0);
        for (automatic, backend, device) in [
            (false, Backend::Dx12, DeviceType::Cpu),
            (true, Backend::Metal, DeviceType::IntegratedGpu),
            (true, Backend::Vulkan, DeviceType::DiscreteGpu),
            (true, Backend::Dx12, DeviceType::VirtualGpu),
            (true, Backend::Gl, DeviceType::Cpu),
        ] {
            let original = adapter(backend, device);
            let selected =
                prefer_hardware_renderer(original.clone(), automatic, Clone::clone, || {
                    attempts.set(attempts.get() + 1);
                    Err::<AdapterInfo, _>("unexpected backend initialization")
                });
            assert_eq!(selected, original);
        }
        assert_eq!(attempts.get(), 0);
    }
}
