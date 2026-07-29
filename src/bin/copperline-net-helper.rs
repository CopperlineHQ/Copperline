// SPDX-License-Identifier: GPL-3.0-or-later

#[cfg(target_os = "linux")]
fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("--serve") if args.next().is_none() => copperline::net::bridge::linux::run_helper(),
        _ => anyhow::bail!(
            "copperline-net-helper is an internal capability helper; \
             Copperline starts it automatically"
        ),
    }
}

#[cfg(not(target_os = "linux"))]
fn main() -> anyhow::Result<()> {
    anyhow::bail!("copperline-net-helper is only used on Linux")
}
