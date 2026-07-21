//! OpenVM host runner for the C++ ray tracer.
//!
//! Three build configurations, selected at compile time via cargo features:
//!   * `softfloat` (default): rv32im only, no FPU. compiler-rt's softfloat.
//!   * `zfinx`:           rv32im_zfinx, native FPU in integer registers.
//!   * `f`:               rv32imf, native FPU in FP register file (AS 5).
//!
//! Pick exactly one via `--features`. Mutually-exclusive combinations are
//! caught by compile_error! below.

#![allow(unexpected_cfgs)]

#[cfg(not(any(feature = "softfloat", feature = "zfinx", feature = "f")))]
compile_error!("Enable exactly one feature: --features softfloat|zfinx|f");
#[cfg(all(feature = "softfloat", any(feature = "zfinx", feature = "f")))]
compile_error!("`softfloat` is mutually exclusive with `zfinx`/`f`");
#[cfg(all(feature = "zfinx", feature = "f"))]
compile_error!("`zfinx` and `f` are mutually exclusive");

use eyre::Context;
use openvm_circuit::arch::{execution_mode::InstructionInfo, Streams, SystemConfig, VmExecutor};
use openvm_circuit::utils::air_test;
use openvm_instructions::exe::VmExe;
use openvm_rv32im_transpiler::{
    Rv32ITranspilerExtension, Rv32IoTranspilerExtension, Rv32MTranspilerExtension,
};
use openvm_sha2_transpiler::Sha2TranspilerExtension;
use openvm_stark_sdk::p3_baby_bear::BabyBear;
use openvm_transpiler::{elf::Elf, openvm_platform::memory::MEM_SIZE, transpiler::Transpiler, FromElf};
use sha2::{Digest, Sha256};
use std::time::Instant;

#[cfg(feature = "softfloat")]
use openvm_sha2_circuit::{Sha2Rv32Config, Sha2Rv32CpuBuilder};

#[cfg(any(feature = "zfinx", feature = "f"))]
mod config;
#[cfg(any(feature = "zfinx", feature = "f"))]
use config::{Rv32FSha2CpuBuilder, Rv32FSha2Config};
#[cfg(any(feature = "zfinx", feature = "f"))]
use openvm_rv32f_transpiler::Rv32FTranspilerExtension;

type F = BabyBear;

#[cfg(feature = "softfloat")]
const DEFAULT_ELF_PATH: &str = "./build/raytracer";
#[cfg(feature = "zfinx")]
const DEFAULT_ELF_PATH: &str = "./build/raytracer";
#[cfg(feature = "f")]
const DEFAULT_ELF_PATH: &str = "./build/raytracer";

const PPM_OUT: &str = "image_openvm.ppm";

const PV_PTR_OFF: usize = 0;
const PV_SIZE_OFF: usize = 4;
const PV_HASH_OFF: usize = 8;
const PV_HASH_LEN: usize = 32;
const NUM_PUBLIC_VALUES: usize = 64;

fn main() -> eyre::Result<()> {
    tracing_subscriber::fmt::init();

    let args: Vec<String> = std::env::args().collect();
    let prove = args.iter().any(|a| a == "prove");
    let elf_path = args
        .iter()
        .skip(1)
        .find(|a| *a != "prove")
        .cloned()
        .unwrap_or_else(|| DEFAULT_ELF_PATH.to_string());

    #[cfg(feature = "softfloat")]
    println!("[config] softfloat (rv32im)");
    #[cfg(feature = "zfinx")]
    println!("[config] zfinx (rv32im_zfinx)");
    #[cfg(feature = "f")]
    println!("[config] F extension (rv32imf, FP regs in AS 5)");
    println!("ELF: {elf_path}");

    // 1. Load and decode the ELF.
    let elf_bytes = std::fs::read(&elf_path)
        .with_context(|| format!("failed to read ELF at {elf_path}"))?;
    let elf = Elf::decode(&elf_bytes, MEM_SIZE as u32)
        .with_context(|| "failed to decode ELF")?;

    // 2. Build the transpiler chain (feature-selected).
    let exe = VmExe::from_elf(elf, build_transpiler())
        .with_context(|| "failed to transpile ELF")?;

    // 3. Build config (feature-selected).
    #[cfg(feature = "softfloat")]
    let mut config = Sha2Rv32Config::default();
    #[cfg(feature = "zfinx")]
    let mut config = Rv32FSha2Config::new_zfinx();
    #[cfg(feature = "f")]
    let mut config = Rv32FSha2Config::new_f();

    enlarge_public_values(&mut config.system);

    // 4. Execute. Use profiling hook when PROFILE_FILE is set.
    let executor = VmExecutor::new(config)?;
    let instance = executor.instance(&exe)?;
    let profile_path = std::env::var("PROFILE_FILE").ok();
    let t = Instant::now();
    let state = if let Some(path) = &profile_path {
        let prof = std::sync::Arc::new(std::sync::Mutex::new(
            openvm_profiler::FoldedProfile::new(&elf_bytes)
                .map_err(|e| eyre::eyre!("failed to parse ELF for profiling: {e}"))?,
        ));
        let prof_hook = prof.clone();
        let state = instance
            .execute_with_hook(
                Streams::<F>::default(),
                Box::new(move |info: InstructionInfo| {
                    prof_hook.lock().unwrap().on_instruction(info.pc)
                }),
            )
            .with_context(|| "execution failed")?;
        let prof = prof.lock().unwrap();
        let mut f = std::fs::File::create(path)
            .with_context(|| format!("failed to create profile file {path}"))?;
        prof.write(&mut f)
            .map_err(|e| eyre::eyre!("failed to write folded profile: {e}"))?;
        println!("profile:    wrote {path}");
        state
    } else {
        instance
            .execute(Streams::<F>::default(), None)
            .with_context(|| "execution failed")?
    };
    let exec_secs = t.elapsed().as_secs_f64();
    println!("execution:   {exec_secs:.2}s");

    // 5. Read public values: ptr, size, hash.
    let pvs: &[u8] =
        unsafe { state.memory.get_slice::<u8>(3, 0, NUM_PUBLIC_VALUES) };
    let ptr = u32::from_le_bytes(
        pvs[PV_PTR_OFF..PV_PTR_OFF + 4].try_into().expect("ptr"),
    );
    let size = u32::from_le_bytes(
        pvs[PV_SIZE_OFF..PV_SIZE_OFF + 4].try_into().expect("size"),
    );
    let revealed_hash = &pvs[PV_HASH_OFF..PV_HASH_OFF + PV_HASH_LEN];

    println!(
        "guest:      PPM at AS2 {ptr:#010x}, {size} bytes, hash {}",
        hex::encode(revealed_hash)
    );

    // 6. Read the rendered PPM bytes out of AS 2.
    if size == 0 || ptr == 0 {
        return Err(eyre::eyre!(
            "guest did not populate public values (ptr={ptr}, size={size})"
        ));
    }
    let ppm: &[u8] =
        unsafe { state.memory.get_slice::<u8>(2, ptr, size as usize) };

    std::fs::write(PPM_OUT, ppm)
        .with_context(|| format!("failed to write {PPM_OUT}"))?;
    println!("wrote:      {PPM_OUT} ({size} bytes)");

    // 7. Verify host SHA-256 matches the revealed hash.
    let mut hasher = Sha256::new();
    hasher.update(ppm);
    let computed = hasher.finalize();
    if computed.as_slice() == revealed_hash {
        println!("sha256:     matches");
    } else {
        eprintln!(
            "sha256:     MISMATCH\n  revealed: {}\n  computed: {}",
            hex::encode(revealed_hash),
            hex::encode(computed),
        );
        return Err(eyre::eyre!("hash mismatch"));
    }

    // 8. Optional: generate and verify a STARK proof.
    if prove {
        println!("\n=== Proving (this may take a while) ===");
        let t = Instant::now();

        #[cfg(feature = "softfloat")]
        {
            let mut config = Sha2Rv32Config::default();
            enlarge_public_values(&mut config.system);
            air_test(Sha2Rv32CpuBuilder, config, exe.clone());
        }

        #[cfg(feature = "zfinx")]
        {
            let mut config = Rv32FSha2Config::new_zfinx();
            enlarge_public_values(&mut config.system);
            air_test(Rv32FSha2CpuBuilder, config, exe.clone());
        }

        #[cfg(feature = "f")]
        {
            let mut config = Rv32FSha2Config::new_f();
            enlarge_public_values(&mut config.system);
            air_test(Rv32FSha2CpuBuilder, config, exe.clone());
        }

        let prove_secs = t.elapsed().as_secs_f64();
        // Read peak memory from /proc/self/status
        let vm_peak_kb: u64 = std::fs::read_to_string("/proc/self/status")
            .ok()
            .and_then(|s| {
                s.lines()
                    .find(|l| l.starts_with("VmPeak:"))
                    .and_then(|l| l.split_whitespace().nth(1).and_then(|n| n.parse().ok()))
            })
            .unwrap_or(0);
        let vm_peak_gb = vm_peak_kb as f64 / 1_048_576.0;
        println!("prove+verify: {prove_secs:.2}s (peak memory: {vm_peak_gb:.1} GB)");
        println!("✓ proof verified");
    }

    Ok(())
}

/// Build the transpiler with extensions appropriate for the active feature.
#[cfg(feature = "softfloat")]
fn build_transpiler() -> Transpiler<F> {
    Transpiler::<F>::default()
        .with_extension(Rv32ITranspilerExtension)
        .with_extension(Rv32MTranspilerExtension)
        .with_extension(Rv32IoTranspilerExtension)
        .with_extension(Sha2TranspilerExtension)
}

#[cfg(feature = "zfinx")]
fn build_transpiler() -> Transpiler<F> {
    Transpiler::<F>::default()
        .with_extension(Rv32ITranspilerExtension)
        .with_extension(Rv32MTranspilerExtension)
        .with_extension(Rv32IoTranspilerExtension)
        .with_extension(Rv32FTranspilerExtension::default())
        .with_extension(Sha2TranspilerExtension)
}

#[cfg(feature = "f")]
fn build_transpiler() -> Transpiler<F> {
    Transpiler::<F>::default()
        .with_extension(Rv32ITranspilerExtension)
        .with_extension(Rv32MTranspilerExtension)
        .with_extension(Rv32IoTranspilerExtension)
        .with_extension(Rv32FTranspilerExtension::new_f())
        .with_extension(Sha2TranspilerExtension)
}

fn enlarge_public_values(system: &mut SystemConfig) {
    system.num_public_values = NUM_PUBLIC_VALUES;
    system.memory_config.addr_spaces[3].num_cells = NUM_PUBLIC_VALUES;
}
