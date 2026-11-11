//! Whether the server's processor offers x86-64-v2 (SSE3, SSSE3, SSE4.1, SSE4.2, POPCNT, CMPXCHG16B, LAHF/SAHF).
//!
//! Prebuilt programs the hotel needs no longer start without it: Node.js modules such as Rollup (the build of the
//! AtomCMS theme) stop with "Illegal instruction", sharp (the EpicNext build) and .NET (the All-in-1 converter) refuse
//! to start. A real x86-64 processor from the last fifteen years has it; it is missing when a VPS shows its virtual
//! CPU as a generic model ("Common KVM processor", qemu64 or kvm64), which the host can change (CPU type "host").

/// The /proc/cpuinfo flags of x86-64-v2, with the names the CPU manuals use.
const X86_64_V2: [(&str, &str); 7] = [
    ("pni", "SSE3"),
    ("ssse3", "SSSE3"),
    ("sse4_1", "SSE4.1"),
    ("sse4_2", "SSE4.2"),
    ("popcnt", "POPCNT"),
    ("cx16", "CMPXCHG16B"),
    ("lahf_lm", "LAHF/SAHF"),
];

/// Prints the machine (`uname -m`) and the flags of the first processor, one per line. Needs no root.
pub const SCRIPT: &str = "uname -m; grep -m1 '^flags' /proc/cpuinfo 2>/dev/null | cut -d: -f2";

/// The x86-64-v2 features the processor lacks, by name. Empty for another architecture (arm64 ...), or when the flags
/// could not be read: only a missing feature that was really seen counts.
pub fn missing_x86_64_v2(machine: &str, flags: &str) -> Vec<&'static str> {
    if machine.trim() != "x86_64" || flags.trim().is_empty() {
        return Vec::new();
    }
    let present: std::collections::HashSet<&str> = flags.split_whitespace().collect();
    X86_64_V2.iter().filter(|(flag, _)| !present.contains(flag)).map(|(_, name)| *name).collect()
}

/// [`missing_x86_64_v2`] from the output of [`SCRIPT`].
pub fn missing_from_output(output: &str) -> Vec<&'static str> {
    let mut lines = output.lines();
    let machine = lines.next().unwrap_or_default();
    let flags = lines.next().unwrap_or_default();
    missing_x86_64_v2(machine, flags)
}

/// The warning for the login log, or None when nothing is missing.
pub fn warning(missing: &[&str]) -> Option<String> {
    if missing.is_empty() {
        return None;
    }
    Some(format!(
        "  WARNING: this processor is not x86-64-v2 (missing: {}). Node.js modules (Rollup for the AtomCMS theme, sharp for \
         EpicNext) and .NET programs will stop with \"Illegal instruction\" or refuse to start. On a VPS the virtual CPU is \
         shown as a generic model: ask the host to set its CPU type to \"host\" (or x86-64-v2 or newer).",
        missing.join(", ")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    // A generic KVM CPU ("Common KVM processor") and a real one, as /proc/cpuinfo lists them.
    const KVM64: &str = " fpu vme de pse tsc msr pae mce cx8 apic sep mtrr pge mca cmov pat pse36 clflush mmx fxsr sse sse2 \
                         ht syscall nx lm constant_tsc nopl xtopology cpuid tsc_known_freq pni cx16 x2apic hypervisor \
                         lahf_lm cpuid_fault pti";
    const XEON: &str = " fpu vme de pse tsc msr pae mce cx8 apic sep mtrr pge mca cmov pat pse36 clflush mmx fxsr sse sse2 \
                        ss ht syscall nx pdpe1gb rdtscp lm constant_tsc rep_good nopl xtopology cpuid pni pclmulqdq ssse3 \
                        fma cx16 pcid sse4_1 sse4_2 x2apic movbe popcnt aes xsave avx f16c rdrand hypervisor lahf_lm abm \
                        3dnowprefetch avx2";

    #[test]
    fn a_generic_kvm_cpu_lacks_x86_64_v2() {
        assert_eq!(missing_x86_64_v2("x86_64", KVM64), vec!["SSSE3", "SSE4.1", "SSE4.2", "POPCNT"]);
        assert!(warning(&missing_x86_64_v2("x86_64", KVM64)).unwrap().contains("SSE4.2, POPCNT"));
    }

    #[test]
    fn a_real_cpu_and_other_machines_are_fine() {
        assert!(missing_x86_64_v2("x86_64", XEON).is_empty());
        assert!(warning(&[]).is_none());
        // Not x86-64, or nothing read: no warning.
        assert!(missing_x86_64_v2("aarch64", "fp asimd evtstrm").is_empty());
        assert!(missing_x86_64_v2("x86_64", "").is_empty());
    }

    #[test]
    fn reads_the_script_output() {
        assert_eq!(missing_from_output(&format!("x86_64\n{KVM64}\n")).len(), 4);
        assert!(missing_from_output(&format!("x86_64\n{XEON}\n")).is_empty());
        assert!(missing_from_output("").is_empty());
    }
}
