use m68k::{CpuCore, CpuType};

#[test]
fn test_reset_disables_translation_and_ttrs_on_68040() {
    let mut cpu = CpuCore::new();
    cpu.set_cpu_type(CpuType::M68040);
    cpu.set_sr(0x2700);

    // Enable translation (040 TC enable is bit 15) and a transparent
    // translation register, as 68040.library does at OS boot.
    cpu.write_control_register(0x003, 0x8000); // TC
    cpu.write_control_register(0x004, 0x0000_8000); // ITT0 enable
    assert!(cpu.pmmu_enabled, "TC write should enable translation");

    cpu.pulse_reset();

    assert!(!cpu.pmmu_enabled, "reset must disable address translation");
    assert_eq!(cpu.mmu_tc, 0, "reset must clear TC");
    assert_eq!(cpu.itt0, 0, "reset must clear the TTR enable bits");
    assert_eq!(cpu.vbr, 0, "reset must clear VBR");
}
