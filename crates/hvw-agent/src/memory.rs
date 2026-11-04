//! The launch admission check on the free memory of the member. The memory
//! of a VM is a ceiling: the VM takes host memory only when it uses it and
//! gives free memory back. Thus the slots do not protect the member from
//! overcommit, and a launch needs enough free memory now.

use anyhow::{Context, Result, bail};

/// MemAvailable of a /proc/meminfo text, in MiB.
pub fn available_mib(meminfo: &str) -> Result<u64> {
    let kb = meminfo
        .lines()
        .find_map(|l| l.strip_prefix("MemAvailable:"))
        .and_then(|v| v.trim().strip_suffix("kB"))
        .and_then(|v| v.trim().parse::<u64>().ok());
    match kb {
        Some(kb) => Ok(kb / 1024),
        None => bail!("/proc/meminfo has no valid MemAvailable line"),
    }
}

/// Fails when the member has less free memory than `min_mib`.
pub fn check_launch(min_mib: u64, meminfo: &str) -> Result<()> {
    if min_mib == 0 {
        return Ok(());
    }
    let avail = available_mib(meminfo)?;
    if avail < min_mib {
        bail!(
            "the member has too little free memory to launch a VM: \
             {avail} MiB available, {min_mib} MiB needed"
        );
    }
    Ok(())
}

/// Reads /proc/meminfo and calls [`check_launch`].
pub fn check_launch_now(min_mib: u64) -> Result<()> {
    if min_mib == 0 {
        return Ok(());
    }
    let meminfo = std::fs::read_to_string("/proc/meminfo").context("cannot read /proc/meminfo")?;
    check_launch(min_mib, &meminfo)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MEMINFO: &str = "\
MemTotal:       49324028 kB
MemFree:         2713936 kB
MemAvailable:   12582912 kB
Buffers:         6070148 kB
Cached:         32066504 kB
SwapCached:            0 kB
Active:          5904056 kB
Inactive:       36604096 kB
SwapTotal:             0 kB
SwapFree:              0 kB
Dirty:           3106032 kB
AnonPages:       4372744 kB
Shmem:              5144 kB
HugePages_Total:       0
Hugepagesize:       2048 kB
DirectMap1G:    49283072 kB
";

    const NO_AVAILABLE: &str = "\
MemTotal:       49324028 kB
MemFree:         2713936 kB
Buffers:         6070148 kB
";

    #[test]
    fn available_is_in_mib() {
        assert_eq!(available_mib(MEMINFO).unwrap(), 12288);
    }

    #[test]
    fn check_passes_at_the_limit_and_fails_above() {
        check_launch(12288, MEMINFO).unwrap();
        let err = check_launch(12289, MEMINFO).unwrap_err();
        assert_eq!(
            err.to_string(),
            "the member has too little free memory to launch a VM: \
             12288 MiB available, 12289 MiB needed"
        );
    }

    #[test]
    fn zero_turns_the_check_off() {
        check_launch(0, NO_AVAILABLE).unwrap();
        check_launch_now(0).unwrap();
    }

    #[test]
    fn missing_line_is_an_error() {
        let err = available_mib(NO_AVAILABLE).unwrap_err();
        assert_eq!(
            err.to_string(),
            "/proc/meminfo has no valid MemAvailable line"
        );
        let bad = "MemAvailable:   lots kB\n";
        assert!(available_mib(bad).is_err());
        assert!(check_launch(1, NO_AVAILABLE).is_err());
    }
}
