//! Memory limits imposed by the cgroup this process runs in.
//!
//! `parse_meminfo` reads `MemTotal`/`MemAvailable` from `/proc/meminfo`, which
//! are facts about the *host*. Inside a limit they are the wrong numbers, and
//! the failure has no soft edge: nothing degrades, the kernel kills the
//! process, so no ladder rung softens it and the only fix is to see the limit.
//! WARP documents the size of it — a 32 GiB cgroup on a 256 GiB host saw a
//! 224 GiB ceiling and asked for 75 GiB.
//!
//! This is desktop-relevant, not only a hosting concern: a Flatpak or a systemd
//! user slice puts a limit on the app on an ordinary Linux desktop.
//!
//! Everything here is a pure function over injected input. That is deliberate.
//! On a normal systemd desktop every level of the hierarchy reports `max`, so a
//! test that reads the host's own `/sys/fs/cgroup` passes while asserting
//! nothing at all.

use std::path::Path;

use serde::Serialize;

const MIB: u64 = 1024 * 1024;

/// cgroup v1 writes a sentinel rather than a word when memory is unlimited:
/// `PAGE_COUNTER_MAX * PAGE_SIZE`, which is `LONG_MAX` rounded down to a page
/// boundary — 9223372036854771712 on a 4 KiB-page kernel, 9223372036854710272
/// on a 64 KiB-page one. The exact value therefore depends on the kernel's page
/// size, so this is a floor to compare against rather than a value to match.
/// 2^62 is four exbibytes: far above any limit anyone will set, far below every
/// sentinel any page size produces.
const V1_UNLIMITED_FLOOR: u64 = 1 << 62;

// The floor has to sit above every limit a human would set and below every
// sentinel a page size can produce. Both halves are checked here, at compile
// time, rather than in a test: the relation is between two constants, so a
// runtime assertion could only ever be vacuously true — clippy's
// `assertions_on_constants` is right about that — and this way changing either
// constant breaks the build instead of passing a test that proves nothing.
const _: () = assert!(V1_UNLIMITED_FLOOR > 1_000_000_000_000_000);
const _: () = assert!(V1_UNLIMITED_FLOOR < 9_223_372_036_854_710_272);

/// What the cgroup hierarchy allows, in MB. `None` means "the hierarchy said
/// nothing" — which is not the same as zero, and is the common case.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CgroupMemory {
    /// Smallest finite limit found at any level. Bounds capacity.
    pub limit_mb: Option<u64>,
    /// Bytes the hierarchy will actually admit right now. Bounds pressure.
    pub headroom_mb: Option<u64>,
}

impl CgroupMemory {
    /// No limit anywhere — the answer on an ordinary desktop, and the answer on
    /// every platform that is not Linux.
    pub const NONE: Self = Self {
        limit_mb: None,
        headroom_mb: None,
    };
}

/// One level of the hierarchy. `limit_bytes` is `None` when that level says
/// unlimited; `current_bytes` is `None` when the usage file could not be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CgroupLevel {
    pub limit_bytes: Option<u64>,
    pub current_bytes: Option<u64>,
}

/// The three states a limit file can be in. Collapsing the first two is the
/// trap: the root cgroup has no `memory.max` file at all — it never does — and
/// a reader that treats a missing file as `0` plans every job at rung 3.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LimitRead {
    /// No such file, or its contents did not parse. The level says nothing.
    Missing,
    /// The file exists and says there is no limit.
    Unlimited,
    Bytes(u64),
}

impl LimitRead {
    fn finite(self) -> Option<u64> {
        match self {
            LimitRead::Bytes(n) => Some(n),
            _ => None,
        }
    }

    fn is_missing(self) -> bool {
        matches!(self, LimitRead::Missing)
    }
}

/// Reduce a hierarchy to the two figures the planner needs.
///
/// **Limit** is the smallest finite value at any level, because the limit is
/// hierarchical and a leaf saying `max` does not cancel a finite parent.
///
/// **Headroom** is the `min` over levels of `limit - current`, *not*
/// `(smallest limit) - (that level's current)`. The distinction is the whole
/// point. In v2 an allocation is charged to the cgroup and to every ancestor,
/// so `X` bytes are admissible only if `current + X <= limit` at every level.
/// TASKS.md writes `effective_free() = min(MemAvailable, usable_ram() -
/// memory.current)`, which computes the second form; on an almost-full parent
/// with a small child — the normal shape of a busy user session — that is wrong
/// in the direction that freezes machines:
///
/// | level  | limit | current | headroom |
/// |--------|-------|---------|----------|
/// | parent | 8 GB  | 7 GB    | **1 GB** |
/// | child  | 6 GB  | 1 GB    | 5 GB     |
///
/// The smallest finite limit is the child's 6 GB, so the spec's form yields
/// `6 - 1 = 5 GB`. The kernel will admit 1 GB. A planner told it has 5 GB when
/// it has 1 GB is precisely the failure this reader exists to prevent,
/// reproduced inside the fix for it. Pinned by
/// `a_nearly_full_parent_beats_a_roomy_child`.
///
/// A level whose limit is readable but whose `current` is not contributes its
/// limit and no headroom figure. Assuming zero usage would overstate free
/// memory, which is the failure being fixed; assuming full usage would badge a
/// healthy machine red over an unreadable file. Skipping it measures what can
/// be measured and leaves the limit still bounding capacity.
pub fn from_levels(levels: &[CgroupLevel]) -> CgroupMemory {
    let limit = levels.iter().filter_map(|l| l.limit_bytes).min();
    let headroom = levels
        .iter()
        .filter_map(|l| Some(l.limit_bytes?.saturating_sub(l.current_bytes?)))
        .min();
    CgroupMemory {
        limit_mb: limit.map(|b| b / MIB),
        headroom_mb: headroom.map(|b| b / MIB),
    }
}

/// `memory.max` / `memory.high`: a byte count, or the word `max`.
pub fn parse_v2_limit(raw: &str) -> LimitRead {
    match raw.trim() {
        "" => LimitRead::Missing,
        "max" => LimitRead::Unlimited,
        n => n
            .parse::<u64>()
            .map_or(LimitRead::Missing, LimitRead::Bytes),
    }
}

/// `memory.limit_in_bytes`: always a number, with a page-size-derived sentinel
/// standing in for "unlimited".
pub fn parse_v1_limit(raw: &str) -> LimitRead {
    match raw.trim().parse::<u64>() {
        Ok(n) if n >= V1_UNLIMITED_FLOOR => LimitRead::Unlimited,
        Ok(n) => LimitRead::Bytes(n),
        Err(_) => LimitRead::Missing,
    }
}

/// Under v2 `/proc/self/cgroup` has exactly one line, `0::<path>`.
pub fn v2_relative_path(proc_self_cgroup: &str) -> Option<&str> {
    proc_self_cgroup
        .lines()
        .find_map(|line| line.strip_prefix("0::"))
        .map(str::trim)
}

/// Under v1 there is a line per controller group, `<id>:<controllers>:<path>`.
/// The memory controller may be co-mounted, so the controller field is a
/// comma-separated list and has to be split rather than compared whole.
pub fn v1_relative_path(proc_self_cgroup: &str) -> Option<&str> {
    proc_self_cgroup.lines().find_map(|line| {
        let mut parts = line.splitn(3, ':');
        let _hierarchy_id = parts.next()?;
        let controllers = parts.next()?;
        let path = parts.next()?;
        controllers
            .split(',')
            .any(|c| c == "memory")
            .then_some(path.trim())
    })
}

fn read_limit(path: &Path, parse: fn(&str) -> LimitRead) -> LimitRead {
    std::fs::read_to_string(path).map_or(LimitRead::Missing, |raw| parse(&raw))
}

fn read_usage(path: &Path) -> Option<u64> {
    std::fs::read_to_string(path)
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()
}

fn v2_level(dir: &Path) -> Option<CgroupLevel> {
    let max = read_limit(&dir.join("memory.max"), parse_v2_limit);
    let high = read_limit(&dir.join("memory.high"), parse_v2_limit);
    if max.is_missing() && high.is_missing() {
        // No memory controller at this level. Distinct from "says max".
        return None;
    }
    // `memory.high` is a throttle rather than a kill boundary, but it is the
    // point past which the kernel reclaims hard enough to destroy throughput,
    // so for planning it counts as a limit and is not ranked below
    // `memory.max`. The smallest finite value of either key wins.
    let limit = [max.finite(), high.finite()].into_iter().flatten().min();
    Some(CgroupLevel {
        limit_bytes: limit,
        current_bytes: read_usage(&dir.join("memory.current")),
    })
}

fn v1_level(dir: &Path) -> Option<CgroupLevel> {
    let limit = read_limit(&dir.join("memory.limit_in_bytes"), parse_v1_limit);
    if limit.is_missing() {
        return None;
    }
    // `memory.soft_limit_in_bytes` is deliberately not read. v1's soft limit is
    // advisory under global pressure only; it is not the throttle boundary that
    // v2's `memory.high` is, and treating it as one would shrink budgets on
    // hosts where it is a hint.
    Some(CgroupLevel {
        limit_bytes: limit.finite(),
        current_bytes: read_usage(&dir.join("memory.usage_in_bytes")),
    })
}

/// Walk from the leaf named by `relative` up to and including `root`.
fn walk(root: &Path, relative: &str, level: fn(&Path) -> Option<CgroupLevel>) -> Vec<CgroupLevel> {
    let mut segments: Vec<&str> = relative
        .trim_start_matches('/')
        .split('/')
        .filter(|s| !s.is_empty() && *s != ".." && *s != ".")
        .collect();
    let mut levels = Vec::new();
    loop {
        let dir = segments
            .iter()
            .fold(root.to_path_buf(), |path, segment| path.join(segment));
        if let Some(found) = level(&dir) {
            levels.push(found);
        }
        if segments.pop().is_none() {
            break;
        }
    }
    levels
}

/// Resolve the hierarchy against injected roots.
///
/// v2 wins whenever it yields any level at all, including a hierarchy that says
/// `max` everywhere: that is an answer, not a gap, and falling through to v1 on
/// it would read a stale co-mounted v1 tree on every ordinary systemd desktop.
/// v1 is consulted only when the unified hierarchy carries no memory controller.
pub fn detect_at(proc_self_cgroup: &str, v2_root: &Path, v1_root: &Path) -> CgroupMemory {
    let v2 = v2_relative_path(proc_self_cgroup)
        .map(|relative| walk(v2_root, relative, v2_level))
        .unwrap_or_default();
    if !v2.is_empty() {
        return from_levels(&v2);
    }
    let v1 = v1_relative_path(proc_self_cgroup)
        .map(|relative| walk(v1_root, relative, v1_level))
        .unwrap_or_default();
    from_levels(&v1)
}

/// Read this process's own hierarchy.
///
/// `/sys/fs/cgroup` is assumed rather than resolved from `/proc/self/mountinfo`:
/// it is the systemd-managed location on every distribution txtCaster targets,
/// and an unreadable path yields [`CgroupMemory::NONE`], which is the same
/// answer as an unlimited one — the host figures stand.
#[cfg(target_os = "linux")]
pub fn detect() -> CgroupMemory {
    let Ok(proc_self_cgroup) = std::fs::read_to_string("/proc/self/cgroup") else {
        return CgroupMemory::NONE;
    };
    detect_at(
        &proc_self_cgroup,
        Path::new("/sys/fs/cgroup"),
        Path::new("/sys/fs/cgroup/memory"),
    )
}

#[cfg(not(target_os = "linux"))]
pub fn detect() -> CgroupMemory {
    CgroupMemory::NONE
}

#[cfg(test)]
mod tests {
    use super::*;

    const GIB: u64 = 1024 * MIB;

    fn level(limit: Option<u64>, current: Option<u64>) -> CgroupLevel {
        CgroupLevel {
            limit_bytes: limit,
            current_bytes: current,
        }
    }

    // -----------------------------------------------------------------
    // The arithmetic over an injected hierarchy.
    // -----------------------------------------------------------------

    /// TASKS.md's `usable_ram() - memory.current` reading, and the refinement
    /// that reads `memory.current` "at the level whose limit won", are both
    /// wrong here and in the same direction. The kernel charges every ancestor,
    /// so the parent's 1 GB is what will actually be admitted.
    #[test]
    fn a_nearly_full_parent_beats_a_roomy_child() {
        let got = from_levels(&[
            level(Some(8 * GIB), Some(7 * GIB)),
            level(Some(6 * GIB), Some(GIB)),
        ]);
        assert_eq!(got.limit_mb, Some(6144), "smallest finite limit");
        assert_eq!(
            got.headroom_mb,
            Some(1024),
            "min over levels of (limit - current); the spec's form yields 5120"
        );
    }

    /// A leaf saying `max` contributes no limit, so the parent's stands. Pins
    /// that an unlimited level is skipped rather than treated as a zero limit.
    #[test]
    fn a_leaf_that_says_max_does_not_cancel_a_finite_parent() {
        let got = from_levels(&[level(None, Some(GIB)), level(Some(4 * GIB), Some(GIB))]);
        assert_eq!(got.limit_mb, Some(4096));
        assert_eq!(got.headroom_mb, Some(3072));
    }

    #[test]
    fn no_finite_limit_anywhere_is_no_answer_rather_than_zero() {
        let got = from_levels(&[level(None, Some(GIB)), level(None, Some(2 * GIB))]);
        assert_eq!(got, CgroupMemory::NONE);
    }

    #[test]
    fn an_empty_hierarchy_is_no_answer() {
        assert_eq!(from_levels(&[]), CgroupMemory::NONE);
    }

    /// A limit with an unreadable `current` still bounds capacity but cannot
    /// bound pressure. Asserting both halves: the limit survives, and the
    /// unreadable level does not get counted as having its whole limit free.
    #[test]
    fn a_level_with_no_readable_usage_keeps_its_limit_and_skips_the_headroom() {
        let got = from_levels(&[level(Some(2 * GIB), None), level(Some(8 * GIB), Some(GIB))]);
        assert_eq!(got.limit_mb, Some(2048), "the tighter limit still wins");
        assert_eq!(
            got.headroom_mb,
            Some(7168),
            "only the measurable level contributes; 2 GiB is not assumed free"
        );
    }

    /// Usage above the limit is a real transient state, and it must clamp at
    /// zero rather than wrap.
    #[test]
    fn usage_over_the_limit_is_zero_headroom_not_an_underflow() {
        let got = from_levels(&[level(Some(GIB), Some(3 * GIB))]);
        assert_eq!(got.headroom_mb, Some(0));
    }

    // -----------------------------------------------------------------
    // File formats.
    // -----------------------------------------------------------------

    #[test]
    fn v2_distinguishes_a_missing_file_from_the_word_max() {
        assert_eq!(parse_v2_limit("max\n"), LimitRead::Unlimited);
        assert_eq!(parse_v2_limit(""), LimitRead::Missing);
        assert_eq!(parse_v2_limit("6442450944\n"), LimitRead::Bytes(6 * GIB));
        assert_eq!(parse_v2_limit("garbage"), LimitRead::Missing);
    }

    /// v1 spells "unlimited" as a page-size-derived sentinel. Both common page
    /// sizes are checked, because the value is not a constant across kernels.
    #[test]
    fn v1_reports_unlimited_as_a_sentinel_rather_than_a_word() {
        assert_eq!(
            parse_v1_limit("9223372036854771712"),
            LimitRead::Unlimited,
            "4 KiB pages"
        );
        assert_eq!(
            parse_v1_limit("9223372036854710272"),
            LimitRead::Unlimited,
            "64 KiB pages"
        );
        assert_eq!(parse_v1_limit("4294967296"), LimitRead::Bytes(4 * GIB));
        // The floor's own bounds are asserted where it is defined, at compile
        // time. Both operands are constants, so asserting the relation here
        // could only ever be vacuously true.
    }

    #[test]
    fn the_v2_path_is_the_only_line_and_the_v1_path_is_the_memory_controllers() {
        let v2 = "0::/user.slice/user-1000.slice/app.scope\n";
        assert_eq!(
            v2_relative_path(v2),
            Some("/user.slice/user-1000.slice/app.scope")
        );
        assert_eq!(v1_relative_path(v2), None, "no controller field to match");

        // Co-mounted controllers: the memory controller shares a line.
        let v1 = "11:freezer:/\n7:cpu,cpuacct,memory:/app\n0::/\n";
        assert_eq!(v1_relative_path(v1), Some("/app"));
        assert_eq!(v2_relative_path(v1), Some("/"));

        let no_memory = "11:freezer:/\n7:cpu,cpuacct:/app\n";
        assert_eq!(v1_relative_path(no_memory), None);
    }

    // -----------------------------------------------------------------
    // Walking a real directory tree. The host's own /sys/fs/cgroup is not
    // used: every level of it reports `max` on an ordinary desktop, so a
    // host-reading test passes while asserting nothing.
    // -----------------------------------------------------------------

    fn write_level(dir: &Path, files: &[(&str, &str)]) {
        std::fs::create_dir_all(dir).expect("create level");
        for (name, value) in files {
            std::fs::write(dir.join(name), value).expect("write file");
        }
    }

    /// The counterexample again, this time through the file walk, with
    /// `memory.high` supplying the child's limit and the child's `memory.max`
    /// saying `max`. Four of TASKS.md's five cgroup cases in one tree: a limit
    /// below MemTotal, a limit set only by `high`, a finite parent under a leaf
    /// that says `max`, and `memory.current` subtracted from it.
    #[test]
    fn a_v2_tree_reads_the_tightest_limit_and_the_tightest_headroom() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        // A real root cgroup has no memory.max file at all.
        write_level(root, &[("memory.current", "12884901888")]);
        write_level(
            &root.join("user.slice"),
            &[
                ("memory.max", "8589934592"),
                ("memory.current", "7516192768"),
            ],
        );
        write_level(
            &root.join("user.slice").join("app.scope"),
            &[
                ("memory.max", "max"),
                ("memory.high", "6442450944"),
                ("memory.current", "1073741824"),
            ],
        );

        let got = detect_at(
            "0::/user.slice/app.scope\n",
            root,
            &root.join("does-not-exist"),
        );
        assert_eq!(got.limit_mb, Some(6144), "the child's memory.high");
        assert_eq!(
            got.headroom_mb,
            Some(1024),
            "the parent's 8 GiB limit minus its 7 GiB current, not the child's 5 GiB"
        );
    }

    /// Both keys finite at one level: the smaller wins regardless of which key
    /// it came from, because `high` is not ranked below `max`.
    #[test]
    fn the_smaller_of_max_and_high_wins_at_the_same_level() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        write_level(
            &root.join("app.scope"),
            &[
                ("memory.max", "8589934592"),
                ("memory.high", "2147483648"),
                ("memory.current", "1073741824"),
            ],
        );
        let got = detect_at("0::/app.scope\n", root, &root.join("nope"));
        assert_eq!(got.limit_mb, Some(2048), "memory.high, the smaller one");
        assert_eq!(got.headroom_mb, Some(1024));
    }

    /// The root's missing `memory.max` must not read as a zero limit — that
    /// would put every job on an ordinary desktop at rung 3.
    #[test]
    fn a_hierarchy_of_missing_and_max_files_reports_no_limit() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        write_level(root, &[("memory.current", "12884901888")]);
        write_level(
            &root.join("app.scope"),
            &[("memory.max", "max"), ("memory.high", "max")],
        );
        let got = detect_at("0::/app.scope\n", root, &root.join("nope"));
        assert_eq!(got, CgroupMemory::NONE);
    }

    #[test]
    fn a_host_with_no_v2_memory_controller_falls_back_to_v1() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let v2_root = tmp.path().join("unified");
        write_level(&v2_root.join("app.scope"), &[("cgroup.procs", "1\n")]);
        let v1_root = tmp.path().join("memory");
        write_level(
            &v1_root,
            &[
                ("memory.limit_in_bytes", "9223372036854771712"),
                ("memory.usage_in_bytes", "0"),
            ],
        );
        write_level(
            &v1_root.join("app"),
            &[
                ("memory.limit_in_bytes", "4294967296"),
                ("memory.usage_in_bytes", "1073741824"),
            ],
        );

        let got = detect_at("0::/app.scope\n7:memory:/app\n", &v2_root, &v1_root);
        assert_eq!(got.limit_mb, Some(4096));
        assert_eq!(got.headroom_mb, Some(3072), "the v1 root is the sentinel");
    }

    /// A v2 hierarchy that says `max` everywhere is an answer, not a gap. If it
    /// fell through to v1 here, every ordinary systemd desktop with a
    /// co-mounted legacy tree would plan against a stale v1 limit.
    #[test]
    fn a_v2_tree_that_says_max_does_not_fall_through_to_v1() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let v2_root = tmp.path().join("unified");
        write_level(&v2_root.join("app.scope"), &[("memory.max", "max")]);
        let v1_root = tmp.path().join("memory");
        write_level(
            &v1_root.join("app"),
            &[
                ("memory.limit_in_bytes", "1073741824"),
                ("memory.usage_in_bytes", "0"),
            ],
        );

        let got = detect_at("0::/app.scope\n7:memory:/app\n", &v2_root, &v1_root);
        assert_eq!(got, CgroupMemory::NONE, "v2 answered; v1 is not consulted");
    }

    /// A path segment cannot escape the injected root.
    #[test]
    fn the_walk_does_not_climb_out_of_its_root() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().join("unified");
        write_level(
            &root,
            &[("memory.max", "2147483648"), ("memory.current", "0")],
        );
        write_level(
            tmp.path(),
            &[("memory.max", "1048576"), ("memory.current", "0")],
        );
        let got = detect_at("0::/../\n", &root, &root);
        assert_eq!(got.limit_mb, Some(2048), "the parent directory is not read");
    }

    /// The whole point of the module, stated as one assertion: this host has no
    /// finite limit, so reading it changes nothing — which is exactly why zero
    /// cgroup awareness survived unnoticed, and why every other test here
    /// injects its tree.
    #[test]
    #[cfg(target_os = "linux")]
    fn detecting_the_real_host_never_panics() {
        let _ = detect();
    }
}
